//! Backend: MIR -> LLVM IR text (bootstrap).

use nexum_mir::{BinOp as MBin, Const, Instr, MirFn, MirModule, Term, UnOp as MUn};
use nexum_sema::Ty;
use nexum_target::TargetSpec;
use std::collections::HashMap;

fn llvm_ty(t: &Ty) -> String {
    match t {
        Ty::Int => "i64".to_string(),
        Ty::Float => "double".to_string(),
        Ty::Bool => "i1".to_string(),
        Ty::Str => "ptr".to_string(),
        Ty::Unit => "void".to_string(),
        Ty::Record(n) => format!("%{}", n),
        Ty::Array(elem, n) => format!("[{} x {}]", n, llvm_ty(elem)),
        _ => "i64".to_string(),
    }
}

pub fn emit(mir: &MirModule, target: &TargetSpec) -> String {
    let mut out = String::new();
    out.push_str("; ModuleID = 'nexum'\n");
    out.push_str(&format!("target triple = \"{}\"\n", target.triple));
    out.push_str(&format!(
        "target datalayout = \"{}\"\n",
        target.data_layout_str()
    ));
    // The runtime slice (panic, managed heap, collector, roots, string
    // helpers) lives in the nexum-runtime crate; see docs/runtime-abi.md.
    let msvc = target.os.contains("windows");
    out.push_str(&nexum_runtime::runtime_ir(msvc));

    // record type definitions
    for (name, fields) in &mir.records {
        let fs: Vec<String> = fields.iter().map(|(_, t)| llvm_ty(t)).collect();
        out.push_str(&format!("%{} = type {{ {} }}\n", name, fs.join(", ")));
    }
    if !mir.records.is_empty() {
        out.push('\n');
    }

    // collect globals (strings)
    let mut globals: Vec<(String, String)> = Vec::new(); // (name, content)
    let mut global_counter = 0usize;
    // We'll do two passes: first collect string constants
    // Simpler: assign global names on demand during emit; but globals must precede use? In LLVM, globals can be defined after use in same module? Usually define before or use forward references allowed for globals? Forward refs to global variables are allowed. We'll collect then prepend.
    // We'll emit functions first into a buffer, tracking new globals, then assemble.

    let mut func_buf = String::new();
    let mut str_globals: HashMap<String, String> = HashMap::new(); // content -> global name
                                                                   // Per-function parameter modes, needed to pass borrows as pointers.
    let sigs: HashMap<String, Vec<bool>> = mir
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.mut_params.clone()))
        .collect();

    for f in &mir.functions {
        emit_fn(
            f,
            &mut func_buf,
            &mut globals,
            &mut global_counter,
            &mut str_globals,
            &sigs,
        );
    }

    // prepend globals
    for (name, content) in &globals {
        // content is raw string; escape
        let escaped = llvm_escape(content);
        let n = content.len() + 1;
        out.push_str(&format!(
            "@{} = private unnamed_addr constant [{} x i8] c\"{}\\00\", align 1\n",
            name, n, escaped
        ));
    }
    // format strings
    // (we'll embed format globals as needed; track separately)
    // For simplicity, ensure format globals exist
    // We'll add them during emit into globals list with special names; handle below by checking names starting fmt_
    out.push('\n');
    out.push_str(&func_buf);
    out
}

fn llvm_escape(s: &str) -> String {
    let mut r = String::new();
    for ch in s.chars() {
        if ch == '\\' {
            r.push_str("\\\\");
        } else if ch == '"' {
            r.push_str("\\22");
        } else if ch == '\n' {
            r.push_str("\\0A");
        } else if ch == '\t' {
            r.push_str("\\09");
        } else if (ch as u32) < 32 || (ch as u32) > 126 {
            r.push_str(&format!("\\{:02X}", ch as u32));
        } else {
            r.push(ch);
        }
    }
    r
}

fn get_str_global(
    content: &str,
    globals: &mut Vec<(String, String)>,
    counter: &mut usize,
    map: &mut HashMap<String, String>,
) -> String {
    if let Some(n) = map.get(content) {
        return n.clone();
    }
    let name = format!("str_{}", *counter);
    *counter += 1;
    globals.push((name.clone(), content.to_string()));
    map.insert(content.to_string(), name.clone());
    name
}

fn get_fmt_global(
    fmt: &str,
    globals: &mut Vec<(String, String)>,
    counter: &mut usize,
    map: &mut HashMap<String, String>,
) -> String {
    // fmt includes no trailing? We'll store full format string
    if let Some(n) = map.get(fmt) {
        return n.clone();
    }
    let name = format!("fmt_{}", *counter);
    *counter += 1;
    globals.push((name.clone(), fmt.to_string()));
    map.insert(fmt.to_string(), name.clone());
    name
}

fn emit_fn(
    f: &MirFn,
    buf: &mut String,
    globals: &mut Vec<(String, String)>,
    gcounter: &mut usize,
    strmap: &mut HashMap<String, String>,
    sigs: &HashMap<String, Vec<bool>>,
) {
    let is_main = f.name == "main";
    let pretty = if is_main {
        "i32".to_string()
    } else {
        llvm_ty(&f.ret)
    };
    let _params: Vec<String> = f
        .params
        .iter()
        .map(|(n, t)| format!("{} %{}", llvm_ty(t), n))
        .collect();
    // But param names may collide with value regs; use %p_name? LLVM allows named params. Use %arg_<name>.
    // `mut` parameters are passed as pointers to the caller's storage.
    let params: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, (n, t))| {
            if f.mut_params.get(i).copied().unwrap_or(false) {
                format!("ptr %arg_{}", n)
            } else {
                format!("{} %arg_{}", llvm_ty(t), n)
            }
        })
        .collect();
    buf.push_str(&format!(
        "define {} @{}({}) {{\n",
        pretty,
        f.name,
        params.join(", ")
    ));
    // map param name -> value? In MIR, param values are Value(0..n) created in lower_fn. Codegen should map Value(i) to %arg_name or to a register.
    // We'll build val_reg map: Value(k) -> "%v{k}"? But params were allocated as fresh values 0..n BEFORE entry block. In codegen we need to bind %v0 = param.
    // Approach: at function start (entry block), emit copies: %v0 = ...? Can't copy param easily. Instead map Value(0) directly to "%arg_name".
    // Build a map from value index to register string.
    let mut valreg: HashMap<usize, String> = HashMap::new();
    for (i, (name, _t)) in f.params.iter().enumerate() {
        valreg.insert(i, format!("%arg_{}", name));
    }
    // value_types
    let vt = &f.value_types;
    // Counter for inline control-flow labels (bounds checks).
    let mut lc = 0usize;

    // ---- Phase 4 memory plan (pre-scan; mirrors the emit order below) ----
    // 1. every `alloca` is hoisted to the prologue, so allocating inside a
    //    loop can never grow the stack;
    // 2. array index reads/writes reuse a hoisted scratch slot per site
    //    (their live range is a single instruction);
    // 3. calls whose result type is `Str` spill the result into a root slot
    //    immediately, so a collection triggered by a later allocation can
    //    see temporary results ("temporary roots");
    // 4. every `Str` alloca (locals and spilled temporaries) is registered in
    //    the function's root frame.
    let mut hoisted: Vec<(usize, Ty)> = Vec::new();
    let mut idx_plan: Vec<(usize, String, String)> = Vec::new(); // (ordinal, reg, llvm ty)
    let mut managed_temps: Vec<usize> = Vec::new(); // dest value ids
    {
        let mut ord = 0usize;
        for b in &f.blocks {
            for in_ in &b.instrs {
                match in_ {
                    Instr::Alloca { dest, ty } => hoisted.push((dest.0, ty.clone())),
                    Instr::IndexGet { base, .. } | Instr::IndexSet { base, .. } => {
                        let bt = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                        idx_plan.push((ord, format!("%isp{}", ord), llvm_ty(&bt)));
                    }
                    Instr::Call {
                        dest: Some(d), ty, ..
                    } if *ty == Ty::Str => managed_temps.push(d.0),
                    _ => {}
                }
                ord += 1;
            }
        }
    }
    let idx_names: HashMap<usize, String> =
        idx_plan.iter().map(|(o, n, _t)| (*o, n.clone())).collect();
    let str_slots: Vec<usize> = hoisted
        .iter()
        .filter(|(_, t)| *t == Ty::Str)
        .map(|(d, _)| *d)
        .collect();
    let mut root_slots: Vec<String> = str_slots.iter().map(|d| format!("%v{}", d)).collect();
    for d in &managed_temps {
        root_slots.push(format!("%v{}_mt", d));
    }
    let has_roots = !root_slots.is_empty();

    let mut ordinal = 0usize;
    let mut first_block = true;

    // emit blocks in order
    for b in &f.blocks {
        buf.push_str(&format!("bb{}:\n", b.id.0));
        if first_block {
            first_block = false;
            for (dest, ty) in &hoisted {
                let r = format!("%v{}", dest);
                valreg.insert(*dest, r.clone());
                buf.push_str(&format!("  {} = alloca {}\n", r, llvm_ty(ty)));
            }
            for (_, reg, ty) in &idx_plan {
                buf.push_str(&format!("  {} = alloca {}, align 16\n", reg, ty));
            }
            for d in &managed_temps {
                buf.push_str(&format!("  %v{}_mt = alloca ptr\n", d));
            }
            // Deterministic roots: zero every root slot before anything can
            // collect, and register the frame.
            for slot in &root_slots {
                buf.push_str(&format!("  store ptr null, ptr {}\n", slot));
            }
            if has_roots {
                let n = root_slots.len();
                buf.push_str("  %nrf = alloca [3 x i64], align 8\n");
                buf.push_str(&format!("  %nrs = alloca [{} x ptr], align 8\n", n));
                for (i, slot) in root_slots.iter().enumerate() {
                    buf.push_str(&format!(
                        "  %nrsi{} = getelementptr [{} x ptr], ptr %nrs, i64 0, i64 {}\n",
                        i, n, i
                    ));
                    buf.push_str(&format!("  store ptr {}, ptr %nrsi{}\n", slot, i));
                }
                buf.push_str("  %nrfp = getelementptr i8, ptr %nrf, i64 8\n");
                buf.push_str("  store ptr %nrs, ptr %nrfp\n");
                buf.push_str("  %nrfc = getelementptr i8, ptr %nrf, i64 16\n");
                buf.push_str(&format!("  store i64 {}, ptr %nrfc\n", n));
                buf.push_str("  call void @nexum_roots_push(ptr %nrf)\n");
            }
        }
        for in_ in &b.instrs {
            match in_ {
                Instr::Const { dest, c, ty: _ } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    match c {
                        Const::Int(v) => buf.push_str(&format!("  {} = add i64 0, {}\n", r, v)),
                        Const::Float(v) => {
                            let ss = if v.is_infinite() {
                                if v.is_sign_positive() {
                                    "0x7FF0000000000000".to_string()
                                } else {
                                    "0xFFF0000000000000".to_string()
                                }
                            } else if v.is_nan() {
                                "0x7FF8000000000000".to_string()
                            } else {
                                let mut s = format!("{}", v);
                                if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                                    s.push_str(".0");
                                }
                                s
                            };
                            buf.push_str(&format!("  {} = fadd double 0.0, {}\n", r, ss));
                        }
                        Const::Bool(v) => buf.push_str(&format!(
                            "  {} = add i1 0, {}\n",
                            r,
                            if *v { 1 } else { 0 }
                        )),
                        Const::Str(s) => {
                            let gname = get_str_global(s, globals, gcounter, strmap);
                            // bitcast global to ptr
                            buf.push_str(&format!(
                                "  {} = getelementptr inbounds [{} x i8], ptr @{}, i64 0, i64 0\n",
                                r,
                                s.len() + 1,
                                gname
                            ));
                        }
                    }
                }
                Instr::BinOp {
                    dest,
                    op,
                    lhs,
                    rhs,
                    ty,
                } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let l = valreg
                        .get(&lhs.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", lhs.0));
                    let rr = valreg
                        .get(&rhs.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", rhs.0));
                    let t = llvm_ty(ty);
                    let oty_str = vt
                        .get(&lhs.0)
                        .map(|x| llvm_ty(x))
                        .unwrap_or_else(|| "i64".to_string());
                    match op {
                        MBin::Add => {
                            if *ty == Ty::Float {
                                buf.push_str(&format!("  {} = fadd {} {}, {}\n", r, t, l, rr));
                            } else {
                                // Int arithmetic wraps (two's complement); no
                                // `nsw` because overflow is defined, not UB.
                                buf.push_str(&format!("  {} = add {} {}, {}\n", r, t, l, rr));
                            }
                        }
                        MBin::Sub => {
                            if *ty == Ty::Float {
                                buf.push_str(&format!("  {} = fsub {} {}, {}\n", r, t, l, rr));
                            } else {
                                buf.push_str(&format!("  {} = sub {} {}, {}\n", r, t, l, rr));
                            }
                        }
                        MBin::Mul => {
                            if *ty == Ty::Float {
                                buf.push_str(&format!("  {} = fmul {} {}, {}\n", r, t, l, rr));
                            } else {
                                buf.push_str(&format!("  {} = mul {} {}, {}\n", r, t, l, rr));
                            }
                        }
                        MBin::Div | MBin::Mod => {
                            if *ty == Ty::Float {
                                let opname = if matches!(op, MBin::Div) {
                                    "fdiv"
                                } else {
                                    "frem"
                                };
                                buf.push_str(&format!(
                                    "  {} = {} double {}, {}\n",
                                    r, opname, l, rr
                                ));
                            } else {
                                // Int division is fully defined: a zero
                                // divisor panics (exit 1); MIN / -1 wraps to
                                // MIN and MIN % -1 is 0.
                                lc += 1;
                                let zero = format!("%v{}_z{}", dest.0, lc);
                                let zb = format!("divz{}", lc);
                                let ob = format!("divok{}", lc);
                                buf.push_str(&format!("  {} = icmp eq i64 {}, 0\n", zero, rr));
                                buf.push_str(&format!(
                                    "  br i1 {}, label %{}, label %{}\n",
                                    zero, zb, ob
                                ));
                                buf.push_str(&format!("{}:\n", zb));
                                let msg =
                                    get_str_global("division by zero", globals, gcounter, strmap);
                                buf.push_str(&format!("  call void @nexum_panic(ptr @{})\n", msg));
                                buf.push_str("  unreachable\n");
                                buf.push_str(&format!("{}:\n", ob));
                                let ismin = format!("%v{}_m{}", dest.0, lc);
                                let isneg1 = format!("%v{}_n{}", dest.0, lc);
                                let ovf = format!("%v{}_o{}", dest.0, lc);
                                let safe_l = format!("%v{}_l{}", dest.0, lc);
                                let q = format!("%v{}_q{}", dest.0, lc);
                                buf.push_str(&format!(
                                    "  {} = icmp eq i64 {}, -9223372036854775808\n",
                                    ismin, l
                                ));
                                buf.push_str(&format!("  {} = icmp eq i64 {}, -1\n", isneg1, rr));
                                buf.push_str(&format!(
                                    "  {} = and i1 {}, {}\n",
                                    ovf, ismin, isneg1
                                ));
                                buf.push_str(&format!(
                                    "  {} = select i1 {}, i64 0, i64 {}\n",
                                    safe_l, ovf, l
                                ));
                                let opname = if matches!(op, MBin::Div) {
                                    "sdiv"
                                } else {
                                    "srem"
                                };
                                buf.push_str(&format!(
                                    "  {} = {} i64 {}, {}\n",
                                    q, opname, safe_l, rr
                                ));
                                let wrap_val = if matches!(op, MBin::Div) {
                                    "-9223372036854775808"
                                } else {
                                    "0"
                                };
                                buf.push_str(&format!(
                                    "  {} = select i1 {}, i64 {}, i64 {}\n",
                                    r, ovf, wrap_val, q
                                ));
                            }
                        }
                        MBin::Eq => {
                            if oty_str == "ptr" {
                                // Byte-wise content equality.
                                let c = format!("%v{}_eq", dest.0);
                                buf.push_str(&format!(
                                    "  {} = call i32 @strcmp(ptr {}, ptr {})\n",
                                    c, l, rr
                                ));
                                buf.push_str(&format!("  {} = icmp eq i32 {}, 0\n", r, c));
                            } else if *ty == Ty::Float {
                                buf.push_str(&format!(
                                    "  {} = fcmp oeq {} {}, {}\n",
                                    r, "double", l, rr
                                ));
                            } else {
                                buf.push_str(&format!(
                                    "  {} = icmp eq {} {}, {}\n",
                                    r, oty_str, l, rr
                                ));
                            }
                        }
                        MBin::Ne => {
                            if oty_str == "ptr" {
                                let c = format!("%v{}_eq", dest.0);
                                buf.push_str(&format!(
                                    "  {} = call i32 @strcmp(ptr {}, ptr {})\n",
                                    c, l, rr
                                ));
                                buf.push_str(&format!("  {} = icmp ne i32 {}, 0\n", r, c));
                            } else {
                                buf.push_str(&format!(
                                    "  {} = icmp ne {} {}, {}\n",
                                    r, oty_str, l, rr
                                ))
                            }
                        }
                        MBin::Lt => {
                            if *ty == Ty::Bool {
                                buf.push_str(&format!(
                                    "  {} = icmp slt {} {}, {}\n",
                                    r, oty_str, l, rr
                                ));
                            } else {
                                buf.push_str(&format!(
                                    "  {} = icmp slt {} {}, {}\n",
                                    r, oty_str, l, rr
                                ));
                            }
                        }
                        MBin::Gt => {
                            buf.push_str(&format!("  {} = icmp sgt {} {}, {}\n", r, oty_str, l, rr))
                        }
                        MBin::Le => {
                            buf.push_str(&format!("  {} = icmp sle {} {}, {}\n", r, oty_str, l, rr))
                        }
                        MBin::Ge => {
                            buf.push_str(&format!("  {} = icmp sge {} {}, {}\n", r, oty_str, l, rr))
                        }
                        MBin::And => buf.push_str(&format!("  {} = and {} {}, {}\n", r, t, l, rr)),
                        MBin::Or => buf.push_str(&format!("  {} = or {} {}, {}\n", r, t, l, rr)),
                    }
                }
                Instr::UnOp {
                    dest,
                    op,
                    operand,
                    ty,
                } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let o = valreg
                        .get(&operand.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", operand.0));
                    match op {
                        MUn::Neg => {
                            if *ty == Ty::Float {
                                buf.push_str(&format!("  {} = fneg double {}\n", r, o));
                            } else {
                                // Wrapping negate: `-MIN` is `MIN`, so no `nsw`.
                                buf.push_str(&format!("  {} = sub i64 0, {}\n", r, o));
                            }
                        }
                        MUn::Not => buf.push_str(&format!("  {} = xor i1 {}, 1\n", r, o)),
                    }
                }
                Instr::Call {
                    dest,
                    callee,
                    args,
                    ty,
                } => {
                    let modes = sigs.get(callee).cloned().unwrap_or_default();
                    let aregs: Vec<String> = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            let r = valreg
                                .get(&a.0)
                                .cloned()
                                .unwrap_or_else(|| format!("%v{}", a.0));
                            if modes.get(i).copied().unwrap_or(false) {
                                // By-reference (`mut`) argument: the value
                                // register holds the caller's slot address.
                                format!("ptr {}", r)
                            } else {
                                let t = vt.get(&a.0).cloned().unwrap_or(Ty::Int);
                                format!("{} {}", llvm_ty(&t), r)
                            }
                        })
                        .collect();
                    if callee == "print" {
                        // determine arg type
                        let argty = args
                            .first()
                            .and_then(|a| vt.get(&a.0).cloned())
                            .unwrap_or(Ty::Int);
                        // Use a format global. We need the actual format string content (with \n). Let's build content string.
                        let fmt_content = match argty {
                            Ty::Int => "%lld\n",
                            Ty::Float => "%g\n",
                            Ty::Bool => "%d\n",
                            Ty::Str => "%s\n",
                            _ => "%lld\n",
                        };
                        let gname = get_fmt_global(fmt_content, globals, gcounter, strmap);
                        let argreg0 = args
                            .first()
                            .map(|a| {
                                valreg
                                    .get(&a.0)
                                    .cloned()
                                    .unwrap_or_else(|| format!("%v{}", a.0))
                            })
                            .unwrap_or_default();
                        let (argreg, argtystr) = if argty == Ty::Bool {
                            let zr = format!("%v{}_z", args.first().map(|a| a.0).unwrap_or(0));
                            buf.push_str(&format!("  {} = zext i1 {} to i64\n", zr, argreg0));
                            (zr, "i64".to_string())
                        } else {
                            (argreg0, llvm_ty(&argty))
                        };
                        buf.push_str(&format!(
                            "  call i32 (ptr, ...) @printf(ptr @{}, {} {})\n",
                            gname, argtystr, argreg
                        ));
                        if let Some(d) = dest {
                            let r = format!("%v{}", d.0);
                            valreg.insert(d.0, r.clone());
                            buf.push_str(&format!("  {} = add i64 0, 0\n", r)); // dummy unit
                        }
                    } else {
                        // Heap builtins name runtime entry points directly.
                        let sym = match callee.as_str() {
                            "concat" => "nexum_concat",
                            "collect" => "nexum_collect",
                            "heap_used" => "nexum_heap_used",
                            "str_char_at" => "nexum_str_char_at",
                            "slice" => "nexum_slice",
                            other => other,
                        };
                        let ret_ty = llvm_ty(ty);
                        let call_txt =
                            format!("  call {} @{}({})\n", ret_ty, sym, aregs.join(", "));
                        if ret_ty == "void" {
                            buf.push_str(&call_txt);
                            if let Some(d) = dest {
                                let r = format!("%v{}", d.0);
                                valreg.insert(d.0, r.clone());
                                buf.push_str(&format!("  {} = add i64 0, 0\n", r));
                            }
                        } else if let Some(d) = dest {
                            let r = format!("%v{}", d.0);
                            valreg.insert(d.0, r.clone());
                            buf.push_str(&format!(
                                "  {} = call {} @{}({})\n",
                                r,
                                ret_ty,
                                sym,
                                aregs.join(", ")
                            ));
                            if *ty == Ty::Str {
                                // Temporary root: spill the fresh result into a
                                // registered slot before any later allocation
                                // can trigger a collection.
                                buf.push_str(&format!("  store ptr {}, ptr %v{}_mt\n", r, d.0));
                            }
                        } else {
                            buf.push_str(&call_txt);
                        }
                    }
                }
                Instr::Record { dest, name, fields } => {
                    let mut cur = "undef".to_string();
                    for (i, fv) in fields.iter().enumerate() {
                        let r = valreg
                            .get(&fv.0)
                            .cloned()
                            .unwrap_or_else(|| format!("%v{}", fv.0));
                        let ft = vt.get(&fv.0).cloned().unwrap_or(Ty::Int);
                        let dst = if i + 1 == fields.len() {
                            format!("%v{}", dest.0)
                        } else {
                            format!("%v{}_r{}", dest.0, i)
                        };
                        buf.push_str(&format!(
                            "  {} = insertvalue %{} {}, {} {}, {}\n",
                            dst,
                            name,
                            cur,
                            llvm_ty(&ft),
                            r,
                            i
                        ));
                        cur = dst;
                    }
                    valreg.insert(dest.0, format!("%v{}", dest.0));
                }
                Instr::FieldGet {
                    dest,
                    base,
                    index,
                    ty: _,
                } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let b = valreg
                        .get(&base.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", base.0));
                    let bt = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                    buf.push_str(&format!(
                        "  {} = extractvalue {} {}, {}\n",
                        r,
                        llvm_ty(&bt),
                        b,
                        index
                    ));
                }
                Instr::FieldSet {
                    dest,
                    base,
                    index,
                    value,
                    ty,
                } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let b = valreg
                        .get(&base.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", base.0));
                    let v = valreg
                        .get(&value.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", value.0));
                    let fty = vt.get(&value.0).cloned().unwrap_or(Ty::Int);
                    buf.push_str(&format!(
                        "  {} = insertvalue {} {}, {} {}, {}\n",
                        r,
                        llvm_ty(ty),
                        b,
                        llvm_ty(&fty),
                        v,
                        index
                    ));
                }
                Instr::Panic { msg } => {
                    let m = valreg
                        .get(&msg.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", msg.0));
                    buf.push_str(&format!("  call void @nexum_panic(ptr {})\n", m));
                }
                Instr::ArrayLit { dest, elems, ty } => {
                    let arr_ty = llvm_ty(ty);
                    let mut cur = "undef".to_string();
                    for (i, ev) in elems.iter().enumerate() {
                        let r = valreg
                            .get(&ev.0)
                            .cloned()
                            .unwrap_or_else(|| format!("%v{}", ev.0));
                        let ety = vt.get(&ev.0).cloned().unwrap_or(Ty::Int);
                        let dst = if i + 1 == elems.len() {
                            format!("%v{}", dest.0)
                        } else {
                            format!("%v{}_a{}", dest.0, i)
                        };
                        buf.push_str(&format!(
                            "  {} = insertvalue {} {}, {} {}, {}\n",
                            dst,
                            arr_ty,
                            cur,
                            llvm_ty(&ety),
                            r,
                            i
                        ));
                        cur = dst;
                    }
                    valreg.insert(dest.0, format!("%v{}", dest.0));
                }
                Instr::IndexGet {
                    dest,
                    base,
                    index,
                    len,
                    ty,
                } => {
                    // Bounds-checked element read. The aggregate base is spilled
                    // to a temporary slot so the element address can be computed
                    // with a dynamic index; LLVM's optimizer removes the round
                    // trip at higher optimization levels.
                    let base_ty = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                    let arr_ty = llvm_ty(&base_ty);
                    let b = valreg
                        .get(&base.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", base.0));
                    let i = valreg
                        .get(&index.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", index.0));
                    lc += 1;
                    let slot = idx_names
                        .get(&ordinal)
                        .cloned()
                        .unwrap_or_else(|| format!("%isp{}", ordinal));
                    let ok = format!("%v{}_ok{}", dest.0, lc);
                    let p = format!("%v{}_p{}", dest.0, lc);
                    let cont = format!("idx_ok{}", lc);
                    let fail = format!("idx_oob{}", lc);
                    buf.push_str(&format!("  store {} {}, ptr {}\n", arr_ty, b, slot));
                    // `ult` catches negative indices too: they wrap to huge
                    // unsigned values and fail the comparison.
                    buf.push_str(&format!("  {} = icmp ult i64 {}, {}\n", ok, i, len));
                    buf.push_str(&format!(
                        "  br i1 {}, label %{}, label %{}\n",
                        ok, cont, fail
                    ));
                    buf.push_str(&format!("{}:\n", fail));
                    let msg = get_str_global("index out of bounds", globals, gcounter, strmap);
                    buf.push_str(&format!("  call void @nexum_panic(ptr @{})\n", msg));
                    buf.push_str("  unreachable\n");
                    buf.push_str(&format!("{}:\n", cont));
                    buf.push_str(&format!(
                        "  {} = getelementptr {}, ptr {}, i64 0, i64 {}\n",
                        p, arr_ty, slot, i
                    ));
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    buf.push_str(&format!("  {} = load {}, ptr {}\n", r, llvm_ty(ty), p));
                }
                Instr::IndexSet {
                    dest,
                    base,
                    index,
                    value,
                    len,
                    ty,
                } => {
                    // Bounds-checked element write; reloads the whole aggregate
                    // afterwards because arrays are value types.
                    let base_ty = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                    let arr_ty = llvm_ty(&base_ty);
                    let b = valreg
                        .get(&base.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", base.0));
                    let i = valreg
                        .get(&index.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", index.0));
                    let v = valreg
                        .get(&value.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", value.0));
                    lc += 1;
                    let slot = idx_names
                        .get(&ordinal)
                        .cloned()
                        .unwrap_or_else(|| format!("%isp{}", ordinal));
                    let ok = format!("%v{}_ok{}", dest.0, lc);
                    let p = format!("%v{}_p{}", dest.0, lc);
                    let cont = format!("idx_ok{}", lc);
                    let fail = format!("idx_oob{}", lc);
                    buf.push_str(&format!("  store {} {}, ptr {}\n", arr_ty, b, slot));
                    buf.push_str(&format!("  {} = icmp ult i64 {}, {}\n", ok, i, len));
                    buf.push_str(&format!(
                        "  br i1 {}, label %{}, label %{}\n",
                        ok, cont, fail
                    ));
                    buf.push_str(&format!("{}:\n", fail));
                    let msg = get_str_global("index out of bounds", globals, gcounter, strmap);
                    buf.push_str(&format!("  call void @nexum_panic(ptr @{})\n", msg));
                    buf.push_str("  unreachable\n");
                    buf.push_str(&format!("{}:\n", cont));
                    buf.push_str(&format!(
                        "  {} = getelementptr {}, ptr {}, i64 0, i64 {}\n",
                        p, arr_ty, slot, i
                    ));
                    buf.push_str(&format!("  store {} {}, ptr {}\n", llvm_ty(ty), v, p));
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    buf.push_str(&format!("  {} = load {}, ptr {}\n", r, arr_ty, slot));
                }
                Instr::Copy { dest, src, ty } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let s = valreg
                        .get(&src.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", src.0));
                    buf.push_str(&format!(
                        "  {} = add {} 0, {}
",
                        r,
                        llvm_ty(ty),
                        s
                    ));
                }
                Instr::Alloca { .. } => {
                    // Hoisted to the prologue by the memory plan above.
                }
                Instr::Load { dest, slot, ty } => {
                    let r = format!("%v{}", dest.0);
                    valreg.insert(dest.0, r.clone());
                    let s = valreg
                        .get(&slot.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", slot.0));
                    buf.push_str(&format!(
                        "  {} = load {}, ptr {}
",
                        r,
                        llvm_ty(ty),
                        s
                    ));
                }
                Instr::Store { slot, value, ty } => {
                    let s = valreg
                        .get(&slot.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", slot.0));
                    let v = valreg
                        .get(&value.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", value.0));
                    buf.push_str(&format!(
                        "  store {} {}, ptr {}
",
                        llvm_ty(ty),
                        v,
                        s
                    ));
                }
            }
            ordinal += 1;
        }
        // terminator
        match &b.term {
            Some(Term::Goto(bb)) => buf.push_str(&format!("  br label %bb{}\n", bb.0)),
            Some(Term::Branch {
                cond,
                then_bb,
                else_bb,
            }) => {
                let c = valreg
                    .get(&cond.0)
                    .cloned()
                    .unwrap_or_else(|| format!("%v{}", cond.0));
                buf.push_str(&format!(
                    "  br i1 {}, label %bb{}, label %bb{}\n",
                    c, then_bb.0, else_bb.0
                ));
            }
            Some(Term::Return(v)) => {
                if has_roots {
                    buf.push_str("  call void @nexum_roots_pop()\n");
                }
                if let Some(val) = v {
                    let r = valreg
                        .get(&val.0)
                        .cloned()
                        .unwrap_or_else(|| format!("%v{}", val.0));
                    let t = vt.get(&val.0).cloned().unwrap_or(f.ret.clone());
                    if is_main {
                        // `main` must return i32 regardless of the declared value type.
                        let conv = format!("%v{}_ret", val.0);
                        match t {
                            Ty::Int => buf.push_str(&format!(
                                "  {} = trunc i64 {} to i32\n  ret i32 {}\n",
                                conv, r, conv
                            )),
                            Ty::Bool => buf.push_str(&format!(
                                "  {} = zext i1 {} to i32\n  ret i32 {}\n",
                                conv, r, conv
                            )),
                            Ty::Float => buf.push_str(&format!(
                                "  {} = fptosi double {} to i32\n  ret i32 {}\n",
                                conv, r, conv
                            )),
                            Ty::Str => buf.push_str("  ret i32 0\n"),
                            _ => buf.push_str("  ret i32 0\n"),
                        }
                    } else if f.ret == Ty::Unit {
                        // Degenerate case: a discarded value in a unit function.
                        buf.push_str("  ret void\n");
                    } else {
                        buf.push_str(&format!("  ret {} {}\n", llvm_ty(&t), r));
                    }
                } else if is_main {
                    buf.push_str("  ret i32 0\n");
                } else {
                    buf.push_str("  ret void\n");
                }
            }
            None => buf.push_str("  unreachable\n"),
            Some(Term::Unreachable) => buf.push_str("  unreachable\n"),
        }
    }
    buf.push_str("}\n\n");
}
