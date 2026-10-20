//! The inline-assembly block, end to end.
//!
//! A program that uses `asm` reaches the machine through one LLVM
//! instruction:
//!
//! ```llvm
//! %r = call i64 asm sideeffect "<template>", "<constraints>"(<operands>)
//! ```
//!
//! Three things about that instruction fail silently. The operands of the
//! call are numbered with the output first, so an argument that is one too
//! many or one too few is a program that keeps running and prints the
//! wrong number. A `~{reg}` clobber is not an argument, so a block that
//! clobbers a register and passes one input still takes one argument. And
//! a function without a frame is entered with the stack at 8 modulo 16,
//! which the system call instruction refuses and which nothing else
//! notices.
//!
//! So the checks here are about the value the block produces, and about
//! the shape of the code that produces it.
//!
//! The shape checks need no toolchain. The behaviour checks need clang:
//! without it they do nothing, and they say so on the standard error.

mod common;

use common::{compile_file, emit_ir, error_lines};
use std::path::PathBuf;
use std::process::Command;

/// The block that copies its input into its output, in the dialect of the
/// host.
///
/// x86-64's inline-assembly dialect is AT&T, on every x86-64 target
/// including Windows; AArch64 has its own syntax, and the register names
/// there carry no sigil.
fn copy_block() -> (&'static str, &'static str) {
    if cfg!(target_arch = "aarch64") {
        ("mov x0, x1", "={x0},{x1}")
    } else {
        ("movq %rdi, %rax", "={rax},{rdi}")
    }
}

/// The program: a block that copies its input into a variable, and the
/// variable is printed.
///
/// The input is a variable and not a constant on purpose. An immediate
/// operand is not portable - LLVM substitutes it as `$5` on x86-64 and as a
/// bare number on AArch64, where it is not a valid immediate - so a
/// constant goes in the text of the block, and an operand is always a
/// register.
fn program() -> String {
    let (template, constraints) = copy_block();
    format!(
        "fn main():\n    let mut out = 0\n    let x = 21\n    asm(out, \"{template}\", \"{constraints}\", x)\n    print(out)\n"
    )
}

/// The program that asks the block for the alignment of the stack.
///
/// The block reads the stack pointer, masks off the low four bits, and
/// leaves the result in the output. A function without a frame is entered
/// with the stack at 8 modulo 16; a function with one at 0 modulo 16.
///
/// The input is a variable the block does not use: the constraint string
/// has to name every operand the program passes, and the count is what
/// the sema checks.
fn alignment_program() -> String {
    let (_, constraints) = copy_block();
    format!(
        "fn main():\n    let mut out = 0\n    let x = 0\n    asm(out, \"movq %rsp, %rax\nandq $$15, %rax\", \"{constraints}\", x)\n    print(out)\n"
    )
}

/// A private work directory for one test.
fn work_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("nexum_tests")
        .join("asm")
        .join(name);
    std::fs::create_dir_all(&d).expect("the work directory");
    d
}

/// The toolchain, or nothing at all.
///
/// A missing clang must not look like a pass, so the checks that need one
/// say what was not checked and do nothing.
fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    match nexum_toolchain::Toolchain::discover() {
        Ok(tc) => Some(tc),
        Err(e) => {
            eprintln!("the assembly block was NOT checked: {e}");
            None
        }
    }
}

/// Compile a program, link it, run it, and return what it printed.
fn run(source: &str, name: &str) -> Option<String> {
    let tc = toolchain()?;
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
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().expect("a path"))
        .unwrap_or_else(|e| panic!("the module must verify:\n{e}\n{ir}"));
    nexum_toolchain::run_clang(
        &tc.clang,
        ll.to_str().expect("a path"),
        exe.to_str().expect("a path"),
    )
    .unwrap_or_else(|e| panic!("the program must link:\n{e}\n{ir}"));
    let out = Command::new(&exe)
        .current_dir(&work)
        .output()
        .expect("the program must run");
    assert!(
        out.status.success(),
        "the program failed: {:?}\n{ir}",
        out.status
    );
    Some(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"))
}

/// The body of a function definition, by its name.
///
/// The definition, not a call to it: a call site has the same name and no
/// body.
fn function_body(ir: &str, name: &str) -> String {
    let marker = format!("@{name}(");
    let mut from = 0;
    let start = loop {
        let i = ir[from..]
            .find(&marker)
            .unwrap_or_else(|| panic!("the module has no {name}"));
        let abs = from + i;
        let line_start = ir[..abs].rfind('\n').map(|p| p + 1).unwrap_or(0);
        if ir[line_start..abs].trim_start().starts_with("define") {
            break abs;
        }
        from = abs + marker.len();
    };
    let rest = &ir[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("the definition of {name} is not terminated"));
    rest[..end].to_string()
}

/// The line of the module that holds the assembly block.
fn asm_line(body: &str) -> String {
    body.lines()
        .find(|l| l.contains("asm sideeffect"))
        .unwrap_or_else(|| panic!("the function has no assembly block:\n{body}"))
        .to_string()
}

/// A program that uses `asm` produces the value the block was asked to
/// produce.
///
/// The block copies its input into the output, and the program prints the
/// output. A mistake in the numbering of the operands is silent: the
/// program keeps running and prints a different number, so the number is
/// the check.
#[test]
fn the_block_produces_the_value() {
    let Some(out) = run(&program(), "value") else {
        return;
    };
    assert_eq!(out, "21\n", "the block did not produce the value");
}

/// The block is one instruction, and the operands of the call are the
/// inputs.
///
/// The output is not an argument: it comes back in the result of the call.
/// A `~{reg}` clobber is not an argument either. So a block with one input
/// takes exactly one argument, whatever the constraint string says about
/// the registers the block clobbers.
#[test]
fn the_block_is_one_call_with_the_inputs_as_the_arguments() {
    let work = work_dir("shape");
    let src = work.join("program.nx");
    std::fs::write(&src, program()).expect("write the program");
    let p = compile_file(&src);
    let errs = error_lines(&p.diagnostics);
    assert!(errs.is_empty(), "the program must compile: {errs:?}");
    let mir = p.mir.expect("the program must lower to MIR");
    let ir = emit_ir(&mir);
    let body = function_body(&ir, "main");
    let line = asm_line(&body);

    // The template is the program's own, unchanged.
    let (template, constraints) = copy_block();
    assert!(
        line.contains(&format!("asm sideeffect \"{template}\"")),
        "the template is not the program's:\n{line}"
    );

    // The constraint string names the output first and the input second.
    let expected_output = if cfg!(target_arch = "aarch64") {
        "={x0}"
    } else {
        "={rax}"
    };
    let expected_input = if cfg!(target_arch = "aarch64") {
        "{x1}"
    } else {
        "{rdi}"
    };
    assert!(
        line.contains(expected_output),
        "the constraint string does not name the output:\n{line}"
    );
    assert!(
        line.contains(expected_input),
        "the constraint string does not name the input:\n{line}"
    );
    assert!(
        line.contains(&format!("\"{constraints}")),
        "the constraint string is not the program's:\n{line}"
    );

    // One argument: the input. The output is not an argument, and a
    // clobber is not an argument either.
    let open = line.find('(').expect("the call has an operand list");
    let close = line[open..].find(')').expect("the operand list is closed");
    let operands = line[open + 1..open + close].trim();
    assert_eq!(
        operands.split(',').count(),
        1,
        "the call does not have one argument:\n{line}"
    );
    assert!(
        operands.starts_with("i64 "),
        "the argument is not an integer:\n{line}"
    );

    // The result of the call is stored into the destination variable.
    let lines: Vec<&str> = body.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("asm sideeffect"))
        .expect("the function has an assembly block");
    let store = lines
        .get(at + 1)
        .unwrap_or_else(|| panic!("the result of the block is not stored:\n{body}"));
    assert!(
        store.contains("store i64"),
        "the result of the block is not stored as an integer:\n{store}"
    );
}

/// The stack is aligned at a block.
///
/// A function without a frame is entered with the stack at 8 modulo 16,
/// and the system call instruction wants 16. The compiler gives a
/// function with a block a frame, so the block sees 0.
#[test]
fn the_stack_is_aligned_at_a_block() {
    if !cfg!(target_arch = "x86_64") {
        // The text of the block is x86-64's; the rule is the same
        // everywhere, but the register is not.
        return;
    }
    let Some(out) = run(&alignment_program(), "alignment") else {
        return;
    };
    assert_eq!(
        out, "0\n",
        "the stack is not aligned at the block: it is at the wrong offset"
    );
}
