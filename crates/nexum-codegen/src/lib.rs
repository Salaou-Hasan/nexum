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
        Ty::List(_) => "ptr".to_string(),
        Ty::Unit => "void".to_string(),
        Ty::Record(n) => format!("%{}", n),
        Ty::Array(elem, n) => format!("[{} x {}]", n, llvm_ty(elem)),
        _ => "i64".to_string(),
    }
}

/// Does this type hold a reference the collector must trace?
fn has_refs(t: &Ty, records: &[(String, Vec<(String, Ty)>)]) -> bool {
    match t {
        Ty::Str | Ty::List(_) => true,
        Ty::Array(elem, _) => has_refs(elem, records),
        Ty::Record(name) => records
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, fields)| fields.iter().any(|(_, ft)| has_refs(ft, records)))
            .unwrap_or(false),
        _ => false,
    }
}

fn llvm_align(t: &Ty, records: &[(String, Vec<(String, Ty)>)]) -> u64 {
    match t {
        Ty::Bool => 1,
        Ty::Array(elem, _) => llvm_align(elem, records),
        Ty::Record(name) => records
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, fields)| {
                fields
                    .iter()
                    .map(|(_, ft)| llvm_align(ft, records))
                    .max()
                    .unwrap_or(1)
            })
            .unwrap_or(1),
        _ => 8,
    }
}

fn llvm_size(t: &Ty, records: &[(String, Vec<(String, Ty)>)]) -> u64 {
    match t {
        Ty::Int | Ty::Float | Ty::Str | Ty::List(_) => 8,
        Ty::Bool => 1,
        Ty::Unit => 0,
        Ty::Array(elem, n) => (*n as u64) * llvm_size(elem, records),
        Ty::Record(name) => records
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, fields)| record_size(fields, records))
            .unwrap_or(0),
        _ => 8,
    }
}

fn record_size(fields: &[(String, Ty)], records: &[(String, Vec<(String, Ty)>)]) -> u64 {
    let mut off: u64 = 0;
    let mut max_align: u64 = 1;
    for (_, ft) in fields {
        let a = llvm_align(ft, records);
        let s = llvm_size(ft, records);
        max_align = max_align.max(a);
        off = off.div_ceil(a) * a;
        off += s;
    }
    off.div_ceil(max_align) * max_align
}

/// Bytes of a value the collector must scan for references. Zero means the
/// value cannot hold a reference and does not need to be a root at all.
fn scan_bytes(t: &Ty, records: &[(String, Vec<(String, Ty)>)]) -> u64 {
    if !has_refs(t, records) {
        return 0;
    }
    match t {
        Ty::Str | Ty::List(_) => 8,
        Ty::Array(elem, n) => (*n as u64) * llvm_size(elem, records),
        Ty::Record(name) => records
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, fields)| record_size(fields, records))
            .unwrap_or(0),
        _ => 0,
    }
}

/// The element type of a `List` value, if it is known.
fn list_elem_ty(t: Option<&Ty>) -> Ty {
    match t {
        Some(Ty::List(elem)) => (**elem).clone(),
        _ => Ty::Unknown,
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
            &mir.records,
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
    // Escape per *byte*: an LLVM `\XX` escape denotes a single byte, so a
    // multi-byte UTF-8 character must be emitted as several escapes.
    let mut r = String::new();
    for &b in s.as_bytes() {
        match b {
            b'\\' => r.push_str("\\\\"),
            b'"' => r.push_str("\\22"),
            b'\n' => r.push_str("\\0A"),
            b'\t' => r.push_str("\\09"),
            b'\r' => r.push_str("\\0D"),
            0x20..=0x7E => r.push(b as char),
            _ => r.push_str(&format!("\\{:02X}", b)),
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
    records: &[(String, Vec<(String, Ty)>)],
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

    // ---- Memory plan (pre-scan; mirrors the emit order below) ----
    // 1. every `alloca` is hoisted to the prologue, so allocating inside a
    //    loop can never grow the stack;
    // 2. array index reads/writes reuse a hoisted scratch slot per site
    //    (their live range is a single instruction);
    // 3. calls whose result holds references spill the result into a root
    //    slot immediately, so a collection triggered by a later allocation
    //    can see temporary results ("temporary roots");
    // 4. `list_push`/`list_set` materialise the element into a hoisted
    //    temporary, because the runtime copies elements by address;
    // 5. every slot that can hold a reference is registered in the function's
    //    root frame together with the number of bytes the collector must
    //    scan (see docs/language-reference/decisions-gc.md).
    let mut hoisted: Vec<(usize, Ty)> = Vec::new();
    let mut idx_plan: Vec<(usize, String, String)> = Vec::new(); // (ordinal, reg, llvm ty)
    let mut managed_temps: Vec<(usize, Ty)> = Vec::new(); // (dest, ty)
    let mut elem_temps: Vec<(usize, String, Ty)> = Vec::new(); // (ordinal, reg, elem ty)
    let mut agg_temps: Vec<(usize, String, Ty)> = Vec::new(); // (ordinal, reg, ty)
    {
        let mut ord = 0usize;
        for b in &f.blocks {
            for in_ in &b.instrs {
                match in_ {
                    Instr::Alloca { dest, ty } => hoisted.push((dest.0, ty.clone())),
                    Instr::IndexGet { base, .. } => {
                        let bt = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                        idx_plan.push((ord, format!("%isp{}", ord), llvm_ty(&bt)));
                    }
                    Instr::IndexSet { base, dest, .. } => {
                        let bt = vt.get(&base.0).cloned().unwrap_or(Ty::Unknown);
                        idx_plan.push((ord, format!("%isp{}", ord), llvm_ty(&bt)));
                        let ty = vt.get(&dest.0).cloned().unwrap_or(Ty::Unknown);
                        if has_refs(&ty, records) {
                            agg_temps.push((ord, format!("%vag{}", ord), ty));
                        }
                    }
                    Instr::Call {
                        dest,
                        callee,
                        args,
                        ty,
                    } => {
                        if let Some(d) = dest
                            && has_refs(ty, records) {
                                managed_temps.push((d.0, ty.clone()));
                            }
                        if matches!(callee.as_str(), "list_push" | "list_set") {
                            let lt = args.first().and_then(|a| vt.get(&a.0)).cloned();
                            let elem = list_elem_ty(lt.as_ref());
                            elem_temps.push((ord, format!("%vle{}", ord), elem));
                        }
                    }
                    // An aggregate value that can hold references must live in
                    // a rooted slot: while it is only in a register, a
                    // collection triggered by a later allocation could free
                    // the strings it points to.
                    Instr::Record { dest, .. }
                    | Instr::ArrayLit { dest, .. }
                    | Instr::FieldSet { dest, .. }
                    | Instr::Copy { dest, .. } => {
                        let ty = vt.get(&dest.0).cloned().unwrap_or(Ty::Unknown);
                        if has_refs(&ty, records) {
                            agg_temps.push((ord, format!("%vag{}", ord), ty));
                        }
                    }
                    _ => {}
                }
                ord += 1;
            }
        }
    }
    let idx_names: HashMap<usize, String> =
        idx_plan.iter().map(|(o, n, _t)| (*o, n.clone())).collect();
    let agg_names: HashMap<usize, String> =
        agg_temps.iter().map(|(o, n, _t)| (*o, n.clone())).collect();
    let elem_names: HashMap<usize, String> = elem_temps
        .iter()
        .map(|(o, n, _t)| (*o, n.clone()))
        .collect();

    // Root entries: (slot address, bytes to scan, type for zero-initialising).
    //
    // Every stack slot is registered, and scalars are scanned as a single
    // word. Registering only the slots that *provably* hold a reference would
    // be more precise, but a single missed slot is a use-after-free, and the
    // collector cannot recover from that. Scanning a numeric slot can only
    // retain an object one cycle too long, which is far cheaper than a
    // dangling pointer. Aggregates are scanned in full.
    let mut root_entries: Vec<(String, u64, Ty)> = Vec::new();
    for (dest, ty) in &hoisted {
        let scan = match ty {
            Ty::Str | Ty::List(_) => 8,
            Ty::Array(_, _) | Ty::Record(_) => llvm_size(ty, records).max(8),
            _ => 8,
        };
        root_entries.push((format!("%v{}", dest), scan, ty.clone()));
    }
    for (dest, ty) in &managed_temps {
        root_entries.push((
            format!("%v{}_mt", dest),
            scan_bytes(ty, records).max(8),
            ty.clone(),
        ));
    }
    for (_, reg, ty) in &elem_temps {
        root_entries.push((reg.clone(), scan_bytes(ty, records).max(8), ty.clone()));
    }
    for (_, reg, ty) in &agg_temps {
        root_entries.push((reg.clone(), scan_bytes(ty, records).max(8), ty.clone()));
    }
    let has_roots = !root_entries.is_empty();

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
            for (dest, ty) in &managed_temps {
                buf.push_str(&format!("  %v{}_mt = alloca {}\n", dest, llvm_ty(ty)));
            }
            for (_, reg, ty) in &elem_temps {
                buf.push_str(&format!("  {} = alloca {}\n", reg, llvm_ty(ty)));
            }
            for (_, reg, ty) in &agg_temps {
                buf.push_str(&format!("  {} = alloca {}\n", reg, llvm_ty(ty)));
            }
            // Zero every root slot so the collector never sees stale bytes,
            // and so the root set is deterministic.
            for (addr, _scan, ty) in &root_entries {
                let lty = llvm_ty(ty);
                if lty == "void" {
                    continue;
                }
                match ty {
                    Ty::Str | Ty::List(_) => {
                        buf.push_str(&format!("  store ptr null, ptr {}\n", addr))
                    }
                    _ => buf.push_str(&format!(
                        "  store {} zeroinitializer, ptr {}\n",
                        lty, addr
                    )),
                }
            }
            if has_roots {
                let n = root_entries.len();
                buf.push_str("  %nrf = alloca %nexum.root_frame, align 8\n");
                buf.push_str(&format!(
                    "  %nrs = alloca [{} x %nexum.root_entry], align 8\n",
                    n
                ));
                for (i, (addr, scan, _ty)) in root_entries.iter().enumerate() {
                    buf.push_str(&format!(
                        "  %nrsi{} = getelementptr [{} x %nexum.root_entry], ptr %nrs, i64 0, i64 {}\n",
                        i, n, i
                    ));
                    buf.push_str(&format!("  store ptr {}, ptr %nrsi{}\n", addr, i));
                    buf.push_str(&format!(
                        "  %nrss{} = getelementptr i8, ptr %nrsi{}, i64 8\n",
                        i, i
                    ));
                    buf.push_str(&format!("  store i64 {}, ptr %nrss{}\n", scan, i));
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
                        .map(llvm_ty)
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
                            buf.push_str(&format!("  {} = icmp slt {} {}, {}\n", r, oty_str, l, rr))
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
                    } else if callee == "list_push" || callee == "list_set" {
                        // The runtime copies elements by address, so the element
                        // is materialised into a hoisted temporary first.
                        let list_reg = valreg
                            .get(&args[0].0)
                            .cloned()
                            .unwrap_or_else(|| format!("%v{}", args[0].0));
                        let elem_ty = list_elem_ty(vt.get(&args[0].0));
                        let ety = llvm_ty(&elem_ty);
                        let tmp = elem_names
                            .get(&ordinal)
                            .cloned()
                            .unwrap_or_else(|| format!("%vle{}", ordinal));
                        if callee == "list_push" {
                            let elem_reg = valreg
                                .get(&args[1].0)
                                .cloned()
                                .unwrap_or_else(|| format!("%v{}", args[1].0));
                            buf.push_str(&format!("  store {} {}, ptr {}\n", ety, elem_reg, tmp));
                            buf.push_str(&format!(
                                "  call void @nexum_list_push(ptr {}, ptr {}, i64 {}, i64 {})\n",
                                list_reg,
                                tmp,
                                llvm_size(&elem_ty, records),
                                scan_bytes(&elem_ty, records)
                            ));
                        } else {
                            let idx_reg = valreg
                                .get(&args[1].0)
                                .cloned()
                                .unwrap_or_else(|| format!("%v{}", args[1].0));
                            let elem_reg = valreg
                                .get(&args[2].0)
                                .cloned()
                                .unwrap_or_else(|| format!("%v{}", args[2].0));
                            buf.push_str(&format!("  store {} {}, ptr {}\n", ety, elem_reg, tmp));
                            buf.push_str(&format!(
                                "  call void @nexum_list_set(ptr {}, i64 {}, ptr {})\n",
                                list_reg, idx_reg, tmp
                            ));
                        }
                        if let Some(d) = dest {
                            let r = format!("%v{}", d.0);
                            valreg.insert(d.0, r.clone());
                            buf.push_str(&format!("  {} = add i64 0, 0\n", r));
                        }
                    } else if callee == "list_get" {
                        let list_reg = valreg
                            .get(&args[0].0)
                            .cloned()
                            .unwrap_or_else(|| format!("%v{}", args[0].0));
                        let idx_reg = valreg
                            .get(&args[1].0)
                            .cloned()
                            .unwrap_or_else(|| format!("%v{}", args[1].0));
                        let ety = llvm_ty(ty);
                        let p = format!("%v{}_p", ordinal);
                        buf.push_str(&format!(
                            "  {} = call ptr @nexum_list_get(ptr {}, i64 {})\n",
                            p, list_reg, idx_reg
                        ));
                        if let Some(d) = dest {
                            let r = format!("%v{}", d.0);
                            valreg.insert(d.0, r.clone());
                            buf.push_str(&format!("  {} = load {}, ptr {}\n", r, ety, p));
                            if has_refs(ty, records) {
                                // The element is loaded out of the list buffer,
                                // so it needs its own root slot.
                                let mt = format!("%v{}_mt", d.0);
                                match ty {
                                    Ty::Str | Ty::List(_) => {
                                        buf.push_str(&format!("  store ptr {}, ptr {}\n", r, mt))
                                    }
                                    _ => buf
                                        .push_str(&format!("  store {} {}, ptr {}\n", ety, r, mt)),
                                }
                            }
                        }
                    } else {
                        // Heap builtins name runtime entry points directly.
                        let sym = match callee.as_str() {
                            "concat" => "nexum_concat",
                            "collect" => "nexum_collect",
                            "heap_used" => "nexum_heap_used",
                            "str_char_at" => "nexum_str_char_at",
                            "slice" => "nexum_slice",
                            "read_file" => "nexum_read_file",
                            "write_file" => "nexum_write_file",
                            "list_new" => "nexum_list_new",
                            "list_len" => "nexum_list_len",
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
                            if has_refs(ty, records) {
                                // Temporary root: spill the fresh result into a
                                // registered slot before any later allocation
                                // can trigger a collection.
                                let mt = format!("%v{}_mt", d.0);
                                match ty {
                                    Ty::Str | Ty::List(_) => {
                                        buf.push_str(&format!("  store ptr {}, ptr {}\n", r, mt))
                                    }
                                    _ => buf.push_str(&format!(
                                        "  store {} {}, ptr {}\n",
                                        ret_ty, r, mt
                                    )),
                                }
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
                    if let Some(reg) = agg_names.get(&ordinal) {
                        let v = format!("%v{}", dest.0);
                        buf.push_str(&format!("  store %{} {}, ptr {}\n", name, v, reg));
                    }                }
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
                    if let Some(reg) = agg_names.get(&ordinal) {
                        buf.push_str(&format!("  store {} {}, ptr {}\n", llvm_ty(ty), r, reg));
                    }
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
                    if let Some(reg) = agg_names.get(&ordinal) {
                        let v = format!("%v{}", dest.0);
                        buf.push_str(&format!("  store {} {}, ptr {}\n", arr_ty, v, reg));
                    }
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
                    if let Some(reg) = agg_names.get(&ordinal) {
                        buf.push_str(&format!(
                            "  store {} {}, ptr {}\n",
                            arr_ty,
                            r,
                            reg
                        ));
                    }
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
                    if let Some(reg) = agg_names.get(&ordinal) {
                        buf.push_str(&format!("  store {} {}, ptr {}\n", llvm_ty(ty), r, reg));
                    }
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
