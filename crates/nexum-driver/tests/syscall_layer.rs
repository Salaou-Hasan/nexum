//! The system call layer's own gate.
//!
//! The layer is the part of the runtime that reaches the kernel: a wrapper
//! per call, the call number in a register, the arguments pinned to the
//! registers the instruction reads them from, and the stack aligned for the
//! call.
//!
//! Every one of those things fails silently. A wrong number is another
//! system call, and the program keeps running until it does something the
//! other call does. A pointer where an integer belongs is the right machine
//! code on one architecture, by accident of the calling convention, and a
//! crash on the next one. A stack at 8 modulo 16 is a fault in the kernel,
//! on a good day. None of them is a compile error, none of them is a
//! message, and the only way to catch them is to look at the shape of the
//! generated code.
//!
//! So this file is mostly about shape. The shape checks run on every
//! platform and for every target the layer covers, because a target that
//! is never executed on the CI is a target whose mistakes nobody sees. The
//! behaviour checks - the file routines, which are the only part of the
//! layer a Nexum program can reach today - need a POSIX host, and are
//! marked as such.
//!
//! Note what this file does *not* do: it does not trust the LLVM
//! verifier. `clang -x ir -c` accepts a module whose call arguments do
//! not match the callee's signature, because clang passes
//! `-disable-llvm-verifier` to the compiler proper. Where a type-level
//! guarantee is needed, the shape of the IR is checked here instead.

mod common;

use common::{compile_file, emit_ir, error_lines, examples_dir, fixtures_dir};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// The targets the system call layer covers.
const POSIX_TARGETS: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
];

/// The target that has no system call layer: Windows has no system call
/// instruction, and its kernel interface lives behind `ntdll`.
const WINDOWS_TARGET: &str = "windows-x86_64";

/// The wrappers the layer provides.
const WRAPPERS: [&str; 8] = [
    "read", "write", "open", "lseek", "close", "mmap", "munmap", "exit",
];

// ---------------------------------------------------------------------------
// The expectations
// ---------------------------------------------------------------------------

/// The system call numbers of one operating system, and the flags that go
/// with them.
///
/// The numbers are the ones the kernel expects, and they are written out
/// here as literals on purpose: a table that is computed from a formula is
/// a table that is wrong in the same way as the code it is compared with.
#[derive(Clone, Copy)]
struct Numbers {
    read: i64,
    write: i64,
    open: i64,
    close: i64,
    lseek: i64,
    mmap: i64,
    munmap: i64,
    exit: i64,
    /// The flags a file is opened with for writing: write only, create it
    /// if it is not there, and truncate it if it is.
    open_write: i64,
    /// The permissions a file that is created gets.
    open_mode: i64,
    /// The protection an anonymous mapping asks for: read and write.
    map_prot: i64,
    /// The flags of an anonymous mapping: private, and not backed by a file.
    map_flags: i64,
}

impl Numbers {
    /// The numbers of one operating system.
    fn for_os(os: &str) -> Numbers {
        if os.contains("macos") {
            Numbers {
                // Darwin numbers its system calls from the BSD master table,
                // and it does not match Linux: read is 3, exit is 1 and
                // lseek is 199 here.
                read: 3,
                write: 4,
                open: 5,
                close: 6,
                lseek: 199,
                mmap: 197,
                munmap: 198,
                exit: 1,
                // 1 | 512 | 1024: write only, create, truncate.
                open_write: 1537,
                open_mode: 420,
                map_prot: 3,
                // 0x2 | 0x1000: private, and not backed by a file.
                map_flags: 4098,
            }
        } else {
            Numbers {
                read: 0,
                write: 1,
                open: 2,
                close: 3,
                lseek: 8,
                mmap: 9,
                munmap: 11,
                exit: 60,
                // 1 | 64 | 512: write only, create, truncate.
                open_write: 577,
                open_mode: 420,
                map_prot: 3,
                // 0x02 | 0x20: private, and not backed by a file.
                map_flags: 34,
            }
        }
    }
}

/// The numbers of a target.
fn numbers_for(target: &str) -> Numbers {
    Numbers::for_os(if target.contains("macos") {
        "macos"
    } else {
        "linux"
    })
}

/// Where the registers of a system call live, for one target.
struct Layout {
    /// The register the system call number goes in.
    number: &'static str,
    /// The instruction that makes the call.
    instruction: &'static str,
    /// The instruction the *other* system uses, which must not be in the
    /// body: `int 0x80` is the way the 32-bit Linux did it, and `svc #0x80`
    /// is Darwin's way on AArch64.
    other_instruction: &'static str,
    /// The registers the instruction reads the arguments from, in order.
    call: &'static [&'static str],
    /// The registers the arguments are pinned to. On x86-64 they are the
    /// registers the instruction reads them from; on AArch64 they are
    /// different ones, because the result comes back in the register the
    /// first argument went in.
    staging: &'static [&'static str],
    /// The register the result comes back in.
    result: &'static str,
    /// The registers the instruction clobbers.
    clobbers: &'static [&'static str],
    /// True when the wrapper has to align the stack itself.
    aligns_stack: bool,
}

/// The layout of a target.
fn layout(target: &str) -> Layout {
    if target.contains("aarch64") {
        if target.contains("macos") {
            Layout {
                number: "x16",
                instruction: "svc #0x80",
                other_instruction: "svc #0",
                call: &["x0", "x1", "x2", "x3", "x4", "x5"],
                staging: &["x9", "x10", "x11", "x12", "x13", "x14"],
                result: "x0",
                clobbers: &["x16"],
                aligns_stack: false,
            }
        } else {
            Layout {
                number: "x8",
                instruction: "svc #0",
                other_instruction: "svc #0x80",
                call: &["x0", "x1", "x2", "x3", "x4", "x5"],
                staging: &["x9", "x10", "x11", "x12", "x13", "x14"],
                result: "x0",
                clobbers: &["x8"],
                aligns_stack: false,
            }
        }
    } else {
        Layout {
            number: "rax",
            instruction: "syscall",
            other_instruction: "int 0x80",
            call: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            staging: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            result: "rax",
            clobbers: &["rcx", "r11", "r12"],
            aligns_stack: true,
        }
    }
}

/// One argument of a system call: where its value comes from.
enum Arg {
    /// The wrapper's own parameter.
    Param(usize),
    /// A constant.
    Const(i64),
}

/// The arguments a wrapper passes to the kernel, in order.
fn wrapper_args(name: &str, n: &Numbers) -> Vec<Arg> {
    match name {
        "read" | "write" | "open" | "lseek" => {
            vec![Arg::Param(0), Arg::Param(1), Arg::Param(2)]
        }
        "close" | "exit" => vec![Arg::Param(0)],
        "munmap" => vec![Arg::Param(0), Arg::Param(1)],
        "mmap" => vec![
            Arg::Param(0),
            Arg::Param(1),
            Arg::Const(n.map_prot),
            Arg::Const(n.map_flags),
            Arg::Const(-1),
            Arg::Const(0),
        ],
        other => panic!("unknown wrapper {other}"),
    }
}

/// The operands a wrapper passes, as they appear in the assembly block.
fn expected_operands(args: &[Arg]) -> Vec<String> {
    args.iter()
        .map(|a| match a {
            Arg::Param(i) => format!("i64 %a{i}"),
            Arg::Const(c) => format!("i64 {c}"),
        })
        .collect()
}

/// The constraint string a wrapper has to have.
///
/// The first constraint is the output: the numbering of the operands of an
/// assembly block counts it as operand zero. The inputs follow, one per
/// argument, each pinned to a register. Then the registers the instruction
/// clobbers, and the memory.
fn expected_constraints(lay: &Layout, args: &[Arg]) -> String {
    let mut parts = vec![format!("={{{}}}", lay.result)];
    for i in 0..args.len() {
        parts.push(format!("{{{}}}", lay.staging[i]));
    }
    for c in lay.clobbers {
        parts.push(format!("~{{{c}}}"));
    }
    parts.push("~{memory}".to_string());
    parts.join(",")
}

/// The number a wrapper puts in the number register.
fn wrapper_number(name: &str, n: &Numbers) -> i64 {
    match name {
        "read" => n.read,
        "write" => n.write,
        "open" => n.open,
        "lseek" => n.lseek,
        "close" => n.close,
        "mmap" => n.mmap,
        "munmap" => n.munmap,
        "exit" => n.exit,
        other => panic!("unknown wrapper {other}"),
    }
}

/// The page size of a target, in bytes.
///
/// This is the layer's contract: the allocator rounds a mapping's length up
/// to it, and Darwin's `munmap` refuses a length that is not a multiple of
/// it, so the wrong constant leaks every large mapping.
///
/// The runtime's own `page_size` function is the implementation of the
/// contract. It is not reachable from here, because `nexum-runtime` is not
/// a dependency of `nexum-driver`, so the table is written out here and
/// checked against the host's real page size by the test below.
fn page_size_of(target: &str) -> i64 {
    match target {
        "macos-aarch64" => 16384,
        _ => 4096,
    }
}

// ---------------------------------------------------------------------------
// Reading the generated code
// ---------------------------------------------------------------------------

/// The program every shape check is run on.
///
/// The runtime is in every module the code generator emits, so any program
/// will do; the smallest one in the repository is used.
fn program() -> &'static nexum_mir::MirModule {
    static PROGRAM: OnceLock<nexum_mir::MirModule> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let p = compile_file(&examples_dir().join("hello.nx"));
        let errs = error_lines(&p.diagnostics);
        assert!(errs.is_empty(), "hello.nx must compile: {errs:?}");
        p.mir.expect("the program must lower to MIR")
    })
}

/// The module for every target, built once.
fn modules() -> &'static HashMap<String, String> {
    static MODULES: OnceLock<HashMap<String, String>> = OnceLock::new();
    MODULES.get_or_init(|| {
        let mut m = HashMap::new();
        for target in nexum_target::TargetSpec::list() {
            let spec = nexum_target::TargetSpec::by_name(target)
                .unwrap_or_else(|| panic!("unknown target {target}"));
            m.insert(target.to_string(), nexum_codegen::emit(program(), &spec));
        }
        m
    })
}

/// The generated module for a target.
fn module_for(target: &str) -> &'static str {
    modules()
        .get(target)
        .map(|s| s.as_str())
        .unwrap_or_else(|| panic!("unknown target {target}"))
}

/// The body of a function definition, by its name.
fn function_body(ir: &str, name: &str) -> String {
    let marker = format!("@{name}(");
    let start = ir
        .find(&marker)
        .unwrap_or_else(|| panic!("the module has no {name}"));
    let rest = &ir[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("the definition of {name} is not terminated"));
    rest[..end].to_string()
}

/// One `asm sideeffect` block, taken apart.
struct AsmBlock {
    /// The assembly body.
    text: String,
    /// The constraint string.
    constraints: String,
    /// The operands.
    operands: String,
}

impl AsmBlock {
    /// The registers the arguments are pinned to: one per input
    /// constraint.
    fn inputs(&self) -> Vec<String> {
        self.constraints
            .split(',')
            .skip(1)
            .filter(|c| !c.starts_with('~'))
            .map(|c| c.trim_matches(|c| c == '{' || c == '}').to_string())
            .collect()
    }

    /// The operands, one per argument.
    fn args(&self) -> Vec<String> {
        if self.operands.trim().is_empty() {
            return Vec::new();
        }
        self.operands
            .split(',')
            .map(|a| a.trim().to_string())
            .collect()
    }
}

/// The assembly blocks of a function body.
fn asm_blocks(body: &str) -> Vec<AsmBlock> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("asm sideeffect \"") {
        rest = &rest[i + "asm sideeffect \"".len()..];
        let (text, r) = rest
            .split_once("\", \"")
            .unwrap_or_else(|| panic!("an assembly block has no constraint string"));
        let (constraints, r) = r
            .split_once("\"(")
            .unwrap_or_else(|| panic!("an assembly block has no operand list"));
        let end = r
            .find(')')
            .unwrap_or_else(|| panic!("an assembly block has no closed operand list"));
        out.push(AsmBlock {
            text: text.to_string(),
            constraints: constraints.to_string(),
            operands: r[..end].to_string(),
        });
        rest = &r[end..];
    }
    out
}

/// The one assembly block of a wrapper.
fn only_asm_block(target: &str, name: &str) -> AsmBlock {
    let body = function_body(module_for(target), &format!("nexum_sys_{name}"));
    let blocks = asm_blocks(&body);
    assert_eq!(
        blocks.len(),
        1,
        "{target}/{name}: {} assembly blocks, expected one",
        blocks.len()
    );
    blocks.into_iter().next().expect("one block")
}

/// The number a wrapper puts in the number register, read back out of the
/// generated assembly.
fn number_in(target: &str, name: &str) -> i64 {
    let lay = layout(target);
    let block = only_asm_block(target, name);
    // On x86-64 the number is an immediate: `movq $$60, %rax`. On AArch64
    // it is `mov x8, #60`.
    let prefix = if lay.aligns_stack {
        "movq $$".to_string()
    } else {
        format!("mov {}, #", lay.number)
    };
    let i = block
        .text
        .find(&prefix)
        .unwrap_or_else(|| panic!("{target}/{name}: no number in {:?}", block.text));
    let rest = &block.text[i + prefix.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|e| panic!("{target}/{name}: {e} in {:?}", block.text))
}

/// The arguments of every call to a system call wrapper, in the module.
fn system_call_arguments(ir: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in ir.lines() {
        let Some(rest) = line.split("call i64 @nexum_sys_").nth(1) else {
            continue;
        };
        let Some(open) = rest.find('(') else { continue };
        let Some(close) = rest[open..].find(')') else {
            continue;
        };
        let list = rest[open + 1..open + close].trim();
        let args = if list.is_empty() {
            Vec::new()
        } else {
            list.split(',').map(|a| a.trim().to_string()).collect()
        };
        out.push(args);
    }
    out
}

/// The lines of an assembly body.
///
/// The text of an assembly block is one line, and the newlines in it are
/// escapes, so the instructions are separated by `\0A`.
fn asm_lines(text: &str) -> Vec<&str> {
    text.split("\\0A").collect()
}

/// Is there an operand reference in an assembly body?
///
/// A single dollar starts an operand reference in an assembly block, and
/// the numbering counts the output as operand zero - so `$0` is the result
/// and not the first argument. A literal dollar is written twice, because
/// a single one starts an operand reference.
fn has_operand_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
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

// ---------------------------------------------------------------------------
// The shape of the layer
// ---------------------------------------------------------------------------

/// The layer is there for the POSIX targets and it is not there for
/// Windows.
///
/// A program on a POSIX target reaches the kernel through the layer and
/// through nothing else; a program on Windows still goes through the C
/// library, because Windows has no system call instruction and its kernel
/// interface lives behind `ntdll`.
#[test]
fn the_layer_is_present_on_the_posix_targets_and_absent_on_windows() {
    for target in POSIX_TARGETS {
        let ir = module_for(target);
        for name in WRAPPERS {
            assert!(
                ir.contains(&format!("define internal i64 @nexum_sys_{name}(")),
                "{target} has no {name} wrapper"
            );
        }
        // The layer is inline assembly, not a call into a library.
        assert!(
            ir.contains("asm sideeffect"),
            "{target} makes no system call at all"
        );
        // and the C library's file routines are not there.
        for symbol in ["fopen", "fclose", "fread", "fwrite", "fseek", "ftell"] {
            assert!(
                !ir.contains(&format!("@{symbol}(")),
                "{target} still calls the C library's {symbol}"
            );
        }
    }

    let ir = module_for(WINDOWS_TARGET);
    assert!(
        !ir.contains("nexum_sys_"),
        "windows has no system call layer, but the module has one"
    );
    assert!(
        !ir.contains("asm sideeffect"),
        "windows has no system call instruction, but the module assembles one"
    );
    for symbol in ["fopen", "fclose", "fread", "fwrite", "fseek", "ftell"] {
        assert!(
            ir.contains(&format!("@{symbol}(")),
            "windows lost the C library's {symbol}"
        );
    }
}

/// The arguments are pinned to their registers, so the body never refers to
/// an operand by number.
///
/// The numbering of the operands of an assembly block counts the output as
/// operand zero, so `$0` is the *result* and not the first argument: a
/// body that refers to an operand by number is one register out, and the
/// mistake is silent until the kernel is reached.
#[test]
fn the_arguments_are_not_numbered_from_zero() {
    for target in POSIX_TARGETS {
        let lay = layout(target);
        for name in WRAPPERS {
            let block = only_asm_block(target, name);
            assert!(
                !has_operand_reference(&block.text),
                "{target}/{name}: the body refers to an operand by number:\n{}",
                block.text
            );
            // Every argument is pinned to a register, and there is one
            // pinned register per operand.
            let args = wrapper_args(name, &numbers_for(target));
            assert_eq!(
                block.inputs().len(),
                args.len(),
                "{target}/{name}: {} pinned registers for {} arguments",
                block.inputs().len(),
                args.len()
            );
            assert_eq!(
                block.args().len(),
                args.len(),
                "{target}/{name}: {} operands for {} arguments",
                block.args().len(),
                args.len()
            );
            for (i, reg) in block.inputs().iter().enumerate() {
                assert_eq!(
                    reg, &lay.staging[i],
                    "{target}/{name}: argument {i} is pinned to {reg}"
                );
            }
            // The whole constraint string: the output, the pinned
            // registers, the clobbers and the memory.
            assert_eq!(
                block.constraints,
                expected_constraints(&lay, &args),
                "{target}/{name}: the constraint string is wrong"
            );
            // And the operands are the ones the wrapper passes.
            assert_eq!(
                block.args(),
                expected_operands(&args),
                "{target}/{name}: the operands are wrong"
            );
        }
    }
}

/// The system call number is an immediate.
///
/// In AT&T syntax `movq 1, %rax` reads the 64-bit value at the address 1,
/// so a number that is not an immediate is a memory access, and the kernel
/// gets a pointer where it expects a call number. A literal dollar is
/// written twice in an assembly block, because a single one starts an
/// operand reference.
#[test]
fn the_number_is_an_immediate() {
    for target in POSIX_TARGETS {
        let lay = layout(target);
        for name in WRAPPERS {
            let block = only_asm_block(target, name);
            let n = wrapper_number(name, &numbers_for(target));
            if lay.aligns_stack {
                assert!(
                    block.text.contains(&format!("movq $${n}, %{}", lay.number)),
                    "{target}/{name}: the number is not an immediate:\n{}",
                    block.text
                );
                // The form that reads memory is not there.
                assert!(
                    !block.text.contains(&format!("movq {n}, %{}", lay.number)),
                    "{target}/{name}: the number is read from memory:\n{}",
                    block.text
                );
                // and the dollar of the immediate is doubled.
                assert!(
                    block.text.contains("$$"),
                    "{target}/{name}: a single dollar starts an operand reference:\n{}",
                    block.text
                );
            } else {
                assert!(
                    block.text.contains(&format!("mov {}, #{n}", lay.number)),
                    "{target}/{name}: the number is not an immediate:\n{}",
                    block.text
                );
                assert!(
                    !block.text.contains(&format!("mov {}, {n}", lay.number)),
                    "{target}/{name}: the number is read from memory:\n{}",
                    block.text
                );
            }
            // The call is made with the instruction the system uses, and
            // not with the one the other system uses. The instructions are
            // compared as whole lines: `svc #0x80` starts with `svc #0`.
            let lines = asm_lines(&block.text);
            assert!(
                lines.contains(&lay.instruction),
                "{target}/{name}: the call is not made with {}:\n{}",
                lay.instruction,
                block.text
            );
            assert!(
                !lines.contains(&lay.other_instruction),
                "{target}/{name}: the call is made with {}:\n{}",
                lay.other_instruction,
                block.text
            );
            // The number is in place before the call is made.
            let number_at = block
                .text
                .find(lay.number)
                .unwrap_or_else(|| panic!("{target}/{name}: no number register"));
            let call_at = block
                .text
                .find(lay.instruction)
                .unwrap_or_else(|| panic!("{target}/{name}: no call instruction"));
            assert!(
                number_at < call_at,
                "{target}/{name}: the number is set after the call:\n{}",
                block.text
            );
        }
    }
}

/// The two systems number their calls differently, and a wrong number is
/// silent: the call reaches the kernel as a different one.
///
/// Darwin's read is 3, which is Linux's close, so a table copied from one
/// system into the other turns every read into a close and says nothing
/// about it.
#[test]
fn the_numbers_are_per_system() {
    // Linux.
    assert_eq!(number_in("linux-x86_64", "read"), 0);
    assert_eq!(number_in("linux-x86_64", "write"), 1);
    assert_eq!(number_in("linux-x86_64", "open"), 2);
    assert_eq!(number_in("linux-x86_64", "close"), 3);
    assert_eq!(number_in("linux-x86_64", "lseek"), 8);
    assert_eq!(number_in("linux-x86_64", "exit"), 60);
    assert_eq!(number_in("linux-x86_64", "mmap"), 9);
    assert_eq!(number_in("linux-x86_64", "munmap"), 11);

    // Darwin.
    assert_eq!(number_in("macos-x86_64", "read"), 3);
    assert_eq!(number_in("macos-x86_64", "write"), 4);
    assert_eq!(number_in("macos-x86_64", "open"), 5);
    assert_eq!(number_in("macos-x86_64", "close"), 6);
    assert_eq!(number_in("macos-x86_64", "lseek"), 199);
    assert_eq!(number_in("macos-x86_64", "exit"), 1);
    assert_eq!(number_in("macos-x86_64", "mmap"), 197);
    assert_eq!(number_in("macos-x86_64", "munmap"), 198);

    // The number belongs to the system, not to the architecture.
    assert_eq!(
        number_in("linux-aarch64", "read"),
        number_in("linux-x86_64", "read")
    );
    assert_eq!(
        number_in("macos-aarch64", "read"),
        number_in("macos-x86_64", "read")
    );
    assert_eq!(
        number_in("macos-aarch64", "exit"),
        number_in("macos-x86_64", "exit")
    );

    // And the two tables are not the same table: Darwin's read is Linux's
    // close, so a table copied from one into the other turns every read
    // into a close.
    assert_eq!(
        number_in("macos-x86_64", "read"),
        number_in("linux-x86_64", "close")
    );
    assert_ne!(
        number_in("linux-x86_64", "read"),
        number_in("macos-x86_64", "read")
    );
    assert_ne!(
        number_in("linux-x86_64", "exit"),
        number_in("macos-x86_64", "exit")
    );
}

/// The stack is aligned for the call, and put back afterwards.
///
/// The system call instruction wants a 16-byte aligned stack, and a
/// function without a frame is entered with the stack at 8 modulo 16, so
/// the wrapper aligns it itself.
#[test]
fn the_stack_is_aligned_for_the_call() {
    for target in POSIX_TARGETS {
        let lay = layout(target);
        for name in WRAPPERS {
            let block = only_asm_block(target, name);
            let text = &block.text;
            if lay.aligns_stack {
                assert!(
                    text.starts_with("mov %rsp, %r12; and $$-16, %rsp; "),
                    "{target}/{name}: the stack is not aligned before the call:\n{text}"
                );
                assert!(
                    text.ends_with("mov %r12, %rsp"),
                    "{target}/{name}: the stack is not put back:\n{text}"
                );
                // The alignment is in place before the call, and the stack is
                // restored after it.
                let align = text.find("and $$-16, %rsp").expect("the stack is aligned");
                let call = text
                    .find(lay.instruction)
                    .expect("there is a call instruction");
                let restore = text.find("mov %r12, %rsp").expect("the stack is restored");
                assert!(
                    align < call,
                    "{target}/{name}: the stack is aligned after the call:\n{text}"
                );
                assert!(
                    call < restore,
                    "{target}/{name}: the stack is restored before the call:\n{text}"
                );
            } else {
                // The AArch64 procedure call standard hands the function a
                // 16-byte aligned stack, so the wrapper must not touch it.
                assert!(
                    !text.contains("and $$-16"),
                    "{target}/{name}: the stack is aligned on a target that does not need it:\n{text}"
                );
                assert!(
                    !text.contains("%rsp"),
                    "{target}/{name}: the stack pointer is touched on a target that does not need it:\n{text}"
                );
            }
        }
    }
}

/// The mapping arguments are in the order the system call expects:
/// address, length, protection, flags, descriptor, offset.
///
/// The flags go in the fourth register and the descriptor in the fifth,
/// and swapping them maps the world with the wrong flags - silently,
/// because a mapping that is wrong still maps.
#[test]
fn the_mapping_arguments_are_in_order() {
    for target in POSIX_TARGETS {
        let lay = layout(target);
        let n = numbers_for(target);
        let block = only_asm_block(target, "mmap");
        let args = wrapper_args("mmap", &n);

        // Six arguments, in the order address, length, protection, flags,
        // descriptor, offset.
        assert_eq!(
            block.args(),
            expected_operands(&args),
            "{target}: the mapping arguments are in the wrong order"
        );
        assert_eq!(args.len(), 6, "{target}: mmap takes six arguments");

        // The i-th argument is pinned to the i-th register, and that
        // register is the i-th register the instruction reads.
        for i in 0..6 {
            assert_eq!(
                block.inputs()[i],
                lay.staging[i],
                "{target}: argument {i} is pinned to the wrong register"
            );
        }
        for i in 0..6 {
            if lay.aligns_stack {
                // On x86-64 the constraint puts the argument in the register
                // the instruction reads it from.
                assert_eq!(
                    lay.staging[i], lay.call[i],
                    "{target}: argument {i} is not in the register the call reads"
                );
            } else {
                // On AArch64 the argument is staged elsewhere and moved
                // into place by the assembly.
                assert!(
                    block
                        .text
                        .contains(&format!("mov {}, {}", lay.call[i], lay.staging[i])),
                    "{target}: argument {i} is not moved into {}:\n{}",
                    lay.call[i],
                    block.text
                );
            }
        }

        // The protection asks for read and write, the descriptor is -1 and
        // the offset is 0, so the mapping is anonymous.
        assert_eq!(
            n.map_prot,
            1 | 2,
            "{target}: the mapping is not read and write"
        );
        assert_eq!(
            block.args()[4],
            "i64 -1",
            "{target}: the mapping is not anonymous"
        );
        assert_eq!(
            block.args()[5],
            "i64 0",
            "{target}: the mapping has an offset"
        );

        // The flags of an anonymous mapping are the system's, and they
        // differ between the systems.
        assert_ne!(
            numbers_for("linux-x86_64").map_flags,
            numbers_for("macos-x86_64").map_flags,
            "the mapping flags are the same on the two systems"
        );

        // The unmapping takes the address and the length, and nothing else:
        // the length has to be a multiple of the page size, and the layer
        // does not round it.
        let unmap = only_asm_block(target, "munmap");
        assert_eq!(
            unmap.args(),
            vec!["i64 %a0".to_string(), "i64 %a1".to_string()],
            "{target}: munmap takes more than the address and the length"
        );
    }
}

/// The page size is 16 KiB on macOS on AArch64 and 4 KiB everywhere else.
///
/// It is not a constant the layer may choose: Darwin's `munmap` refuses a
/// length that is not a multiple of the page size, so the wrong constant
/// leaks every large mapping.
#[test]
fn the_page_size_is_per_target() {
    for target in nexum_target::TargetSpec::list() {
        let expected = if *target == "macos-aarch64" {
            16384
        } else {
            4096
        };
        assert_eq!(
            page_size_of(target),
            expected,
            "{target} has the wrong page size"
        );
    }

    // The host agrees with the table: the page size is a property of the
    // system, not of the layer.
    if cfg!(target_os = "windows") {
        // The Windows build has no system call layer, so the page size of
        // the host is the C library's business and not this gate's.
        return;
    }
    let host = host_target();
    if host == "linux-aarch64" {
        // A Linux kernel on AArch64 may use 4 KiB, 16 KiB or 64 KiB pages,
        // and which one it uses is the kernel's choice, not the target
        // profile's. The layer rounds to the one the profile names.
        return;
    }
    let reported = host_page_size();
    assert_eq!(
        reported,
        page_size_of(host),
        "the host {host} reports a page size of {reported}, and the layer rounds to {}",
        page_size_of(host)
    );
}

/// The system call wrappers take their arguments as integers, so a pointer
/// has to be converted before it is passed.
///
/// The LLVM verifier does not catch a call whose arguments do not match
/// the callee's signature: `clang -x ir -c` accepts it, because clang
/// passes `-disable-llvm-verifier` to the compiler proper. The machine
/// code is right on one architecture by accident of the calling
/// convention, and wrong on the next one. This gate is the type-level
/// guarantee the verifier does not give.
#[test]
fn the_system_calls_are_passed_integers() {
    for target in POSIX_TARGETS {
        let calls = system_call_arguments(module_for(target));
        assert!(
            calls.len() >= 8,
            "{target}: only {} system calls in the module",
            calls.len()
        );
        for args in &calls {
            for arg in args {
                assert!(
                    arg.starts_with("i64 "),
                    "{target}: a system call is passed {arg}, and the wrappers take integers"
                );
            }
        }
    }
}

/// The file routines go through the system call layer on the POSIX targets,
/// and through the C library on Windows.
///
/// The POSIX path converts the path to an integer before it is passed,
/// because the wrappers take integers, and it opens the file with the
/// flags the system wants: none for reading, and write, create and
/// truncate for writing.
#[test]
fn the_file_routines_go_through_the_system_call_layer() {
    for target in POSIX_TARGETS {
        let ir = module_for(target);
        let n = numbers_for(target);

        let read = function_body(ir, "nexum_read_file");
        assert!(
            read.contains("ptrtoint ptr %path to i64"),
            "{target}: the path is not converted before it is passed"
        );
        assert!(
            read.contains("call i64 @nexum_sys_open(i64 %pathi, i64 0, i64 0)"),
            "{target}: the file is not opened for reading:\n{read}"
        );
        // The size is asked for with a seek to the end, and the file is
        // wound back before it is read.
        assert!(
            read.contains("call i64 @nexum_sys_lseek(i64 %fd, i64 0, i64 2)"),
            "{target}: the size of the file is not asked for:\n{read}"
        );
        assert!(
            read.contains("call i64 @nexum_sys_lseek(i64 %fd, i64 0, i64 0)"),
            "{target}: the file is not wound back:\n{read}"
        );
        assert!(
            read.contains("call i64 @nexum_sys_read(i64 %fd, i64 %bufi, i64 %size)"),
            "{target}: the file is not read:\n{read}"
        );
        assert!(
            read.contains("call i64 @nexum_sys_close(i64 %fd)"),
            "{target}: the file is not closed:\n{read}"
        );

        let write = function_body(ir, "nexum_write_file");
        assert!(
            write.contains(&format!(
                "call i64 @nexum_sys_open(i64 %pathi, i64 {}, i64 {})",
                n.open_write, n.open_mode
            )),
            "{target}: the file is not opened for writing:\n{write}"
        );
        assert!(
            write.contains("call i64 @nexum_sys_write(i64 %fd, i64 %contenti, i64 %len)"),
            "{target}: the file is not written:\n{write}"
        );
        assert!(
            write.contains("call i64 @nexum_sys_close(i64 %fd)"),
            "{target}: the file is not closed:\n{write}"
        );
    }

    // The two systems open a file for writing with different flags, and
    // the flags mean the same thing: write only, create it, and truncate
    // it.
    assert_eq!(
        numbers_for("linux-x86_64").open_write,
        1 | 64 | 512,
        "linux"
    );
    assert_eq!(
        numbers_for("macos-x86_64").open_write,
        1 | 512 | 1024,
        "darwin"
    );
    assert_ne!(
        numbers_for("linux-x86_64").open_write,
        numbers_for("macos-x86_64").open_write,
        "the two systems open a file for writing the same way"
    );

    // Windows has no system call instruction, so the file routines go
    // through the C library there.
    let ir = module_for(WINDOWS_TARGET);
    let read = function_body(ir, "nexum_read_file");
    assert!(
        read.contains("call ptr @fopen(ptr %path, ptr @nexum_mode_r)"),
        "windows: the file is not opened through the C library:\n{read}"
    );
    assert!(
        read.contains("call i64 @fread(ptr %buf, i64 1, i64 %size, ptr %f)"),
        "windows: the file is not read through the C library:\n{read}"
    );
    let write = function_body(ir, "nexum_write_file");
    assert!(
        write.contains("call ptr @fopen(ptr %path, ptr @nexum_mode_w)"),
        "windows: the file is not opened through the C library:\n{write}"
    );
    assert!(
        write.contains("call i64 @fwrite(ptr %content, i64 1, i64 %len, ptr %f)"),
        "windows: the file is not written through the C library:\n{write}"
    );
}

// ---------------------------------------------------------------------------
// The behaviour of the layer
// ---------------------------------------------------------------------------

/// A private directory for one test.
fn work_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("nexum_tests")
        .join("syscall_layer")
        .join(name);
    std::fs::create_dir_all(&d).expect("the work directory");
    d
}

/// The toolchain, or a failure that says what is missing.
///
/// A missing clang must not look like a pass: the gate would have proved
/// nothing, and the log would say that it passed.
fn toolchain() -> nexum_toolchain::Toolchain {
    nexum_toolchain::Toolchain::discover()
        .unwrap_or_else(|e| panic!("the system call layer's behaviour was NOT checked: {e}"))
}

/// Compile a program, link it and run it, and return its standard output.
fn run(source: &str, name: &str) -> String {
    let work = work_dir(name);
    let src = work.join("program.nx");
    std::fs::write(&src, source).expect("write the program");
    let p = compile_file(&src);
    let errs = error_lines(&p.diagnostics);
    assert!(errs.is_empty(), "the program must compile: {errs:?}");
    let mir = p.mir.expect("the program must lower to MIR");
    let ir = emit_ir(&mir);
    let ll = work.join("program.ll");
    let exe = work.join("program.exe");
    std::fs::write(&ll, &ir).expect("write the ir");
    let tc = toolchain();
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().expect("a path"))
        .expect("the module must verify");
    nexum_toolchain::run_clang(
        &tc.clang,
        ll.to_str().expect("a path"),
        exe.to_str().expect("a path"),
    )
    .expect("the program must link");
    let out = Command::new(&exe)
        .current_dir(&work)
        .output()
        .expect("the program must run");
    assert!(out.status.success(), "the program failed: {:?}", out.status);
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The file routines write a file.
///
/// The bytes on disk are the ones the layer wrote: a wrong flag, a wrong
/// length or a wrong descriptor puts something else there, and the program
/// that wrote it says nothing.
#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "the system call layer is only executed on a POSIX host; on Windows the file routines go through the C library"
)]
fn the_file_routines_write_a_file() {
    let content = "the bytes the layer wrote";
    let out = run(
        &format!("fn main():\n    write_file(\"written_by_the_layer.tmp\", \"{content}\")\n"),
        "write",
    );
    assert_eq!(out, "", "the program writes no output");
    let work = work_dir("write");
    let written = std::fs::read(work.join("written_by_the_layer.tmp"))
        .expect("the layer must have written the file");
    assert_eq!(
        String::from_utf8_lossy(&written),
        content,
        "the file on disk is not what the program wrote"
    );
}

/// The file routines read a file.
///
/// The file is written by the host, so the read is of a file the layer did
/// not create, and the bytes that come back are the bytes that were
/// there.
#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "the system call layer is only executed on a POSIX host; on Windows the file routines go through the C library"
)]
fn the_file_routines_read_a_file() {
    let content = "the bytes the host wrote";
    let work = work_dir("read");
    std::fs::write(work.join("read_by_the_layer.tmp"), content).expect("write the input");
    let out = run(
        "fn main():\n    let s = read_file(\"read_by_the_layer.tmp\")\n    print(s)\n    print(len(s))\n",
        "read",
    );
    assert_eq!(
        out,
        format!("{content}\n{}\n", content.len()),
        "the file was not read back as it was written"
    );
}

/// The file routines round trip one: what is written is read back.
///
/// The program in the repository that does this is the one the run
/// fixtures use, and it is run here through the layer, on a host that
/// executes the layer.
#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "the system call layer is only executed on a POSIX host; on Windows the file routines go through the C library"
)]
fn the_file_routines_round_trip() {
    let work = work_dir("round_trip");
    let source = fixtures_dir("run").join("io_roundtrip.nx");
    let text = std::fs::read_to_string(&source).expect("the fixture must be there");
    let out = run(&text, "round_trip");
    assert_eq!(
        out, "hello file\n10\n1\n",
        "the file did not survive the round trip"
    );
    let written = std::fs::read(work.join("nexum_io_roundtrip.tmp"))
        .expect("the layer must have written the file");
    assert_eq!(
        String::from_utf8_lossy(&written),
        "hello file",
        "the file on disk is not what the program wrote"
    );
}

/// The name of the target the host is.
fn host_target() -> &'static str {
    let host = nexum_target::TargetSpec::host();
    match (host.os.as_str(), host.arch.as_str()) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        (os, arch) => panic!("no target name for {os}/{arch}"),
    }
}

/// The page size the host operating system reports.
fn host_page_size() -> i64 {
    let out = Command::new("getconf")
        .arg("PAGESIZE")
        .output()
        .unwrap_or_else(|e| {
            panic!("the host's page size could not be read, so the table was not checked against it: {e}")
        });
    assert!(
        out.status.success(),
        "getconf PAGESIZE failed, so the table was not checked against the host"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .parse()
        .unwrap_or_else(|e| panic!("the host's page size is not a number: {e}"))
}
