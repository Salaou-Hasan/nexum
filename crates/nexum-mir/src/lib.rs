//! MIR: control-flow graph, SSA values, effects, verifiers (bootstrap).

use nexum_diagnostics::Diagnostic;
use nexum_parser::BinOp as PB;
use nexum_parser::UnOp as SU;
use nexum_sema::{HirExpr, HirFn, HirModule, HirStmt, Ty};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub usize);

#[derive(Debug, Clone)]
pub enum Const {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Instr {
    Const {
        dest: Value,
        c: Const,
        ty: Ty,
    },
    BinOp {
        dest: Value,
        op: BinOp,
        lhs: Value,
        rhs: Value,
        ty: Ty,
    },
    UnOp {
        dest: Value,
        op: UnOp,
        operand: Value,
        ty: Ty,
    },
    Call {
        dest: Option<Value>,
        callee: String,
        args: Vec<Value>,
        ty: Ty,
    },
    Copy {
        dest: Value,
        src: Value,
        ty: Ty,
    },
    Alloca {
        dest: Value,
        ty: Ty,
    },
    Load {
        dest: Value,
        slot: Value,
        ty: Ty,
    },
    Store {
        slot: Value,
        value: Value,
        ty: Ty,
    },
    Record {
        dest: Value,
        name: String,
        fields: Vec<Value>,
    },
    FieldGet {
        dest: Value,
        base: Value,
        index: usize,
        ty: Ty,
    },
    FieldSet {
        dest: Value,
        base: Value,
        index: usize,
        value: Value,
        ty: Ty,
    },
    Panic {
        msg: Value,
    },
    ArrayLit {
        dest: Value,
        elems: Vec<Value>,
        /// The array type (`Ty::Array(elem, n)`).
        ty: Ty,
    },
    IndexGet {
        dest: Value,
        base: Value,
        index: Value,
        len: usize,
        /// The element type.
        ty: Ty,
    },
    IndexSet {
        dest: Value,
        base: Value,
        index: Value,
        value: Value,
        len: usize,
        /// The element type.
        ty: Ty,
    },
}

#[derive(Debug, Clone)]
pub enum Term {
    Goto(BlockId),
    Branch {
        cond: Value,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    Return(Option<Value>),
    Unreachable,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub instrs: Vec<Instr>,
    pub term: Option<Term>,
}

#[derive(Debug, Clone)]
pub struct MirFn {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    /// Parallel to `params`: `true` means the parameter is passed by reference
    /// (a pointer to the caller's slot, for `mut` parameters).
    pub mut_params: Vec<bool>,
    pub ret: Ty,
    pub locals: Vec<(String, Ty)>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    pub value_types: HashMap<usize, Ty>,
}

#[derive(Debug, Clone)]
pub struct MirModule {
    /// Record definitions: name -> fields (field name, field type).
    pub records: Vec<(String, Vec<(String, Ty)>)>,
    pub functions: Vec<MirFn>,
}

pub struct LowerResult {
    pub module: MirModule,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lower(hir: &HirModule) -> LowerResult {
    // Signature modes, used to decide how call arguments are passed.
    let fn_mut: HashMap<String, Vec<bool>> = hir
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.mut_params.clone()))
        .collect();
    let mut fns = Vec::new();
    for hf in &hir.functions {
        fns.push(lower_fn(hf, &hir.records, &fn_mut));
    }
    LowerResult {
        module: MirModule {
            records: hir.records.clone(),
            functions: fns,
        },
        diagnostics: Vec::new(),
    }
}

struct LowerCtx {
    locals: Vec<(String, Ty)>,
    blocks: Vec<BasicBlock>,
    cur: usize,
    counter: usize,
    env: HashMap<String, Value>,
    value_types: HashMap<usize, Ty>,
    records: Vec<(String, Vec<(String, Ty)>)>,
    /// Function name -> per-parameter by-reference flags.
    fn_mut: HashMap<String, Vec<bool>>,
    terminated: bool,
}

impl LowerCtx {
    fn fresh(&mut self, ty: Ty) -> Value {
        let v = Value(self.counter);
        self.counter += 1;
        self.value_types.insert(v.0, ty);
        v
    }
    fn new_block(&mut self) -> usize {
        let id = self.blocks.len();
        self.blocks.push(BasicBlock {
            id: BlockId(id),
            instrs: Vec::new(),
            term: None,
        });
        self.terminated = false;
        id
    }
    fn push_instr(&mut self, in_: Instr) {
        self.blocks[self.cur].instrs.push(in_);
    }
    fn set_term(&mut self, t: Term) {
        self.blocks[self.cur].term = Some(t);
        self.terminated = true;
    }
}

fn lower_fn(
    hf: &HirFn,
    records: &[(String, Vec<(String, Ty)>)],
    fn_mut: &HashMap<String, Vec<bool>>,
) -> MirFn {
    let mut ctx = LowerCtx {
        locals: hf.params.clone(),
        blocks: Vec::new(),
        cur: 0,
        counter: 0,
        env: HashMap::new(),
        value_types: HashMap::new(),
        records: records.to_vec(),
        fn_mut: fn_mut.clone(),
        terminated: false,
    };
    // param values: allocate first N values (indices 0..n) for codegen mapping
    let mut param_vals: Vec<(String, Ty, Value)> = Vec::new();
    for (name, ty) in &hf.params {
        let v = ctx.fresh(ty.clone());
        param_vals.push((name.clone(), ty.clone(), v));
    }
    ctx.new_block(); // entry block 0
    for (i, (name, ty, pv)) in param_vals.iter().enumerate() {
        let by_ref = hf.mut_params.get(i).copied().unwrap_or(false);
        if by_ref {
            // `mut` parameters arrive as a pointer to the caller's slot: no
            // copy is made and loads/stores go through the caller's storage.
            ctx.locals.push((name.clone(), ty.clone()));
            ctx.env.insert(name.clone(), *pv);
        } else {
            let slot = ctx.fresh(Ty::Int);
            ctx.push_instr(Instr::Alloca {
                dest: slot,
                ty: ty.clone(),
            });
            ctx.push_instr(Instr::Store {
                slot,
                value: *pv,
                ty: ty.clone(),
            });
            ctx.locals.push((name.clone(), ty.clone()));
            ctx.env.insert(name.clone(), slot);
        }
    }
    lower_block_stmts(&mut ctx, &hf.body);
    // Reachable fallthrough on the last block -> implicit return.
    if ctx.blocks[ctx.cur].term.is_none() {
        let term = if hf.ret == Ty::Unit || hf.ret == Ty::Unknown || hf.name == "main" {
            Term::Return(None)
        } else {
            // Falling off the end of a value-returning function yields a zero
            // value of the return type (documented bootstrap behavior).
            let v = ctx.fresh(hf.ret.clone());
            let c = match hf.ret {
                Ty::Float => Const::Float(0.0),
                Ty::Bool => Const::Bool(false),
                _ => Const::Int(0),
            };
            ctx.push_instr(Instr::Const {
                dest: v,
                c,
                ty: hf.ret.clone(),
            });
            Term::Return(Some(v))
        };
        ctx.set_term(term);
    }
    // Fill any remaining unterminated blocks (unreachable code) with Unreachable.
    for b in &mut ctx.blocks {
        if b.term.is_none() {
            b.term = Some(Term::Unreachable);
        }
    }
    // Ensure the function has a return if it falls through on the entry-reachable path.
    // (Already handled: reachable blocks end with Return set during lowering.)
    MirFn {
        name: hf.name.clone(),
        params: hf.params.clone(),
        mut_params: hf.mut_params.clone(),
        ret: hf.ret.clone(),
        locals: ctx.locals.clone(),
        blocks: ctx.blocks,
        entry: BlockId(0),
        value_types: ctx.value_types,
    }
}

fn lower_block_stmts(ctx: &mut LowerCtx, stmts: &[HirStmt]) {
    for s in stmts {
        lower_stmt(ctx, s);
    }
}

fn lower_stmt(ctx: &mut LowerCtx, s: &HirStmt) {
    if ctx.blocks[ctx.cur].term.is_some() {
        return; // dead code after terminator
    }
    match s {
        HirStmt::Let {
            name, ty, value, ..
        } => {
            let v = lower_expr(ctx, value);
            let slot = ctx.fresh(Ty::Int);
            ctx.push_instr(Instr::Alloca {
                dest: slot,
                ty: ty.clone(),
            });
            ctx.push_instr(Instr::Store {
                slot,
                value: v,
                ty: ty.clone(),
            });
            ctx.locals.push((name.clone(), ty.clone()));
            ctx.env.insert(name.clone(), slot);
        }
        HirStmt::Assign { name, value } => {
            let never = value.ty() == Ty::Never;
            let v = lower_expr(ctx, value);
            if let Some(slot) = ctx.env.get(name).cloned() {
                let ty = ctx.value_types.get(&v.0).cloned().unwrap_or(Ty::Int);
                ctx.push_instr(Instr::Store { slot, value: v, ty });
            }
            if never {
                ctx.set_term(Term::Unreachable);
            }
        }
        HirStmt::Expr(e) => {
            let never = e.ty() == Ty::Never;
            lower_expr(ctx, e);
            if never {
                ctx.set_term(Term::Unreachable);
            }
        }
        HirStmt::Return { value } => {
            let v = value.as_ref().map(|e| lower_expr(ctx, e));
            ctx.set_term(Term::Return(v));
        }
        HirStmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let c = lower_expr(ctx, cond);
            let then_id = ctx.new_block();
            let else_id = ctx.new_block();
            let merge_id = ctx.new_block();
            ctx.set_term(Term::Branch {
                cond: c,
                then_bb: BlockId(then_id),
                else_bb: BlockId(else_id),
            });
            // Bindings introduced inside a branch are scoped to it: restore
            // the environment so a merged block sees the outer bindings.
            let saved = ctx.env.clone();
            ctx.cur = then_id;
            lower_block_stmts(ctx, then_branch);
            if ctx.blocks[ctx.cur].term.is_none() {
                ctx.set_term(Term::Goto(BlockId(merge_id)));
            }
            ctx.env = saved.clone();
            ctx.cur = else_id;
            if let Some(eb) = else_branch {
                lower_block_stmts(ctx, eb);
            }
            if ctx.blocks[ctx.cur].term.is_none() {
                ctx.set_term(Term::Goto(BlockId(merge_id)));
            }
            ctx.env = saved;
            ctx.cur = merge_id;
        }
        HirStmt::While { cond, body } => {
            let header = ctx.new_block();
            let body_id = ctx.new_block();
            let exit_id = ctx.new_block();
            if ctx.blocks[ctx.cur].term.is_none() {
                ctx.set_term(Term::Goto(BlockId(header)));
            }
            ctx.cur = header;
            let c = lower_expr(ctx, cond);
            ctx.set_term(Term::Branch {
                cond: c,
                then_bb: BlockId(body_id),
                else_bb: BlockId(exit_id),
            });
            ctx.cur = body_id;
            let saved = ctx.env.clone();
            lower_block_stmts(ctx, body);
            ctx.env = saved;
            if ctx.blocks[ctx.cur].term.is_none() {
                ctx.set_term(Term::Goto(BlockId(header)));
            }
            ctx.cur = exit_id;
        }
        HirStmt::For {
            var,
            start,
            end,
            body,
        } => {
            // The loop variable is scoped to the loop.
            let saved_env = ctx.env.clone();
            let sv = lower_expr(ctx, start);
            let slot = ctx.fresh(Ty::Int);
            ctx.push_instr(Instr::Alloca {
                dest: slot,
                ty: Ty::Int,
            });
            ctx.push_instr(Instr::Store {
                slot,
                value: sv,
                ty: Ty::Int,
            });
            ctx.locals.push((var.clone(), Ty::Int));
            ctx.env.insert(var.clone(), slot);
            let header = ctx.new_block();
            let body_id = ctx.new_block();
            let exit_id = ctx.new_block();
            if ctx.blocks[ctx.cur].term.is_none() {
                ctx.set_term(Term::Goto(BlockId(header)));
            }
            ctx.cur = header;
            let v = ctx.fresh(Ty::Int);
            ctx.push_instr(Instr::Load {
                dest: v,
                slot,
                ty: Ty::Int,
            });
            let e = lower_expr(ctx, end);
            let cmp = ctx.fresh(Ty::Bool);
            ctx.push_instr(Instr::BinOp {
                dest: cmp,
                op: BinOp::Lt,
                lhs: v,
                rhs: e,
                ty: Ty::Bool,
            });
            ctx.set_term(Term::Branch {
                cond: cmp,
                then_bb: BlockId(body_id),
                else_bb: BlockId(exit_id),
            });
            ctx.cur = body_id;
            lower_block_stmts(ctx, body);
            if ctx.blocks[ctx.cur].term.is_none() {
                let v2 = ctx.fresh(Ty::Int);
                ctx.push_instr(Instr::Load {
                    dest: v2,
                    slot,
                    ty: Ty::Int,
                });
                let one = ctx.fresh(Ty::Int);
                ctx.push_instr(Instr::Const {
                    dest: one,
                    c: Const::Int(1),
                    ty: Ty::Int,
                });
                let nv = ctx.fresh(Ty::Int);
                ctx.push_instr(Instr::BinOp {
                    dest: nv,
                    op: BinOp::Add,
                    lhs: v2,
                    rhs: one,
                    ty: Ty::Int,
                });
                ctx.push_instr(Instr::Store {
                    slot,
                    value: nv,
                    ty: Ty::Int,
                });
                ctx.set_term(Term::Goto(BlockId(header)));
            }
            ctx.cur = exit_id;
            ctx.env = saved_env;
        }
    }
}

fn field_index(ctx: &LowerCtx, record: &str, field: &str) -> usize {
    ctx.records
        .iter()
        .find(|(n, _)| n == record)
        .and_then(|(_, fs)| fs.iter().position(|(f, _)| f == field))
        .unwrap_or(0)
}

fn lower_expr(ctx: &mut LowerCtx, e: &HirExpr) -> Value {
    use nexum_sema::HirExpr as HE;
    match e {
        HE::Int(v, ty) => {
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::Const {
                dest: d,
                c: Const::Int(*v),
                ty: ty.clone(),
            });
            d
        }
        HE::Float(v, ty) => {
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::Const {
                dest: d,
                c: Const::Float(*v),
                ty: ty.clone(),
            });
            d
        }
        HE::Bool(v, ty) => {
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::Const {
                dest: d,
                c: Const::Bool(*v),
                ty: ty.clone(),
            });
            d
        }
        HE::Str(v, ty) => {
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::Const {
                dest: d,
                c: Const::Str(v.clone()),
                ty: ty.clone(),
            });
            d
        }
        HE::Var(n, ty) => {
            if let Some(slot) = ctx.env.get(n).copied() {
                let d = ctx.fresh(ty.clone());
                ctx.push_instr(Instr::Load {
                    dest: d,
                    slot,
                    ty: ty.clone(),
                });
                d
            } else {
                let d = ctx.fresh(ty.clone());
                d
            }
        }
        HE::Array(elems, ty) => {
            let mut ev = Vec::new();
            for el in elems {
                ev.push(lower_expr(ctx, el));
            }
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::ArrayLit {
                dest: d,
                elems: ev,
                ty: ty.clone(),
            });
            d
        }
        HE::Index { base, index, ty } => {
            let b = lower_expr(ctx, base);
            let i = lower_expr(ctx, index);
            let bt = ctx.value_types.get(&b.0).cloned().unwrap_or(Ty::Unknown);
            let len = if let Ty::Array(_, n) = bt { n } else { 1 };
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::IndexGet {
                dest: d,
                base: b,
                index: i,
                len,
                ty: ty.clone(),
            });
            d
        }
        HE::IndexSet {
            base,
            index,
            value,
            ty,
        } => {
            let b = lower_expr(ctx, base);
            let i = lower_expr(ctx, index);
            let v = lower_expr(ctx, value);
            let bt = ctx.value_types.get(&b.0).cloned().unwrap_or(Ty::Unknown);
            let len = if let Ty::Array(_, n) = bt { n } else { 1 };
            // `ty` is the array type; the instruction carries the element type.
            let elem_ty = if let Ty::Array(e, _) = ty {
                (**e).clone()
            } else {
                ty.clone()
            };
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::IndexSet {
                dest: d,
                base: b,
                index: i,
                value: v,
                len,
                ty: elem_ty,
            });
            d
        }
        HE::Record { name, args, ty } => {
            let mut fv = Vec::new();
            for a in args {
                fv.push(lower_expr(ctx, a));
            }
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::Record {
                dest: d,
                name: name.clone(),
                fields: fv,
            });
            d
        }
        HE::Field { base, name, ty } => {
            let b = lower_expr(ctx, base);
            let bt = ctx.value_types.get(&b.0).cloned().unwrap_or(Ty::Unknown);
            let idx = if let Ty::Record(n) = &bt {
                field_index(ctx, n, name)
            } else {
                0
            };
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::FieldGet {
                dest: d,
                base: b,
                index: idx,
                ty: ty.clone(),
            });
            d
        }
        HE::FieldSet {
            base,
            name,
            value,
            ty,
        } => {
            let b = lower_expr(ctx, base);
            let v = lower_expr(ctx, value);
            let bt = ctx.value_types.get(&b.0).cloned().unwrap_or(Ty::Unknown);
            let idx = if let Ty::Record(n) = &bt {
                field_index(ctx, n, name)
            } else {
                0
            };
            let d = ctx.fresh(ty.clone());
            ctx.push_instr(Instr::FieldSet {
                dest: d,
                base: b,
                index: idx,
                value: v,
                ty: ty.clone(),
            });
            d
        }
        HE::Unary { op, expr, ty } => {
            let e = lower_expr(ctx, expr);
            let d = ctx.fresh(ty.clone());
            let uo = match op {
                SU::Neg => UnOp::Neg,
                SU::Not => UnOp::Not,
            };
            ctx.push_instr(Instr::UnOp {
                dest: d,
                op: uo,
                operand: e,
                ty: ty.clone(),
            });
            d
        }
        HE::Binary { op, lhs, rhs, ty } => {
            let l = lower_expr(ctx, lhs);
            let r = lower_expr(ctx, rhs);
            let d = ctx.fresh(ty.clone());
            let bo = match op {
                PB::Add => BinOp::Add,
                PB::Sub => BinOp::Sub,
                PB::Mul => BinOp::Mul,
                PB::Div => BinOp::Div,
                PB::Mod => BinOp::Mod,
                PB::Eq => BinOp::Eq,
                PB::Ne => BinOp::Ne,
                PB::Lt => BinOp::Lt,
                PB::Gt => BinOp::Gt,
                PB::Le => BinOp::Le,
                PB::Ge => BinOp::Ge,
                PB::And => BinOp::And,
                PB::Or => BinOp::Or,
            };
            ctx.push_instr(Instr::BinOp {
                dest: d,
                op: bo,
                lhs: l,
                rhs: r,
                ty: ty.clone(),
            });
            d
        }
        HE::Call { callee, args, ty } => {
            // Arguments for `mut` parameters are borrows: pass the caller's
            // slot address instead of the loaded value.
            let modes = ctx.fn_mut.get(callee).cloned().unwrap_or_default();
            let mut av = Vec::new();
            for (i, a) in args.iter().enumerate() {
                let by_ref = modes.get(i).copied().unwrap_or(false);
                if by_ref {
                    if let HE::Var(n, vt) = a {
                        if let Some(slot) = ctx.env.get(n).copied() {
                            // Keep the value type entry intact for loads/stores.
                            if !ctx.value_types.contains_key(&slot.0) {
                                ctx.value_types.insert(slot.0, vt.clone());
                            }
                            av.push(slot);
                            continue;
                        }
                    }
                }
                av.push(lower_expr(ctx, a));
            }
            if callee == "panic" {
                let msg = av.first().copied().unwrap_or(Value(0));
                ctx.push_instr(Instr::Panic { msg });
                return Value(0);
            }
            let d = if *ty == Ty::Unit {
                None
            } else {
                Some(ctx.fresh(ty.clone()))
            };
            ctx.push_instr(Instr::Call {
                dest: d,
                callee: callee.clone(),
                args: av,
                ty: ty.clone(),
            });
            d.unwrap_or(Value(0))
        }
    }
}

// verify module (basic): every block has term; value types present
pub fn verify(m: &MirModule) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for f in &m.functions {
        for b in &f.blocks {
            if b.term.is_none() {
                diags.push(Diagnostic::error(
                    "M001",
                    format!("block {} in {} has no terminator", b.id.0, f.name),
                ));
            }
        }
    }
    diags
}

// ---- Phase 5 optimization passes ---------------------------------------

/// Constant folding: replace pure `BinOp`/`UnOp`/`Copy` instructions whose
/// operands are compile-time constants with a `Const` instruction. Operands
/// that are themselves folded constants are resolved transitively.
pub fn const_fold(m: &mut MirModule) {
    for f in &mut m.functions {
        // value -> constant, built from the current (pre-fold) instructions.
        let mut consts: HashMap<Value, Const> = HashMap::new();
        for b in &f.blocks {
            for in_ in &b.instrs {
                if let Instr::Const { dest, c, .. } = in_ {
                    consts.insert(*dest, c.clone());
                }
            }
        }
        // Iterate to a fixpoint: folding one instruction may enable the next.
        loop {
            let mut changed = false;
            for b in &mut f.blocks {
                for in_ in &mut b.instrs {
                    match in_ {
                        Instr::BinOp {
                            dest,
                            op,
                            lhs,
                            rhs,
                            ty,
                        } => {
                            if let (Some(cl), Some(cr)) =
                                (consts.get(lhs).cloned(), consts.get(rhs).cloned())
                            {
                                if let Some(fc) = fold_binop(*op, &cl, &cr, ty.clone()) {
                                    consts.insert(*dest, fc.clone());
                                    *in_ = Instr::Const {
                                        dest: *dest,
                                        c: fc,
                                        ty: ty.clone(),
                                    };
                                    changed = true;
                                }
                            }
                        }
                        Instr::UnOp {
                            dest,
                            op,
                            operand,
                            ty,
                        } => {
                            if let Some(c) = consts.get(operand).cloned() {
                                if let Some(fc) = fold_unop(*op, &c, ty.clone()) {
                                    consts.insert(*dest, fc.clone());
                                    *in_ = Instr::Const {
                                        dest: *dest,
                                        c: fc,
                                        ty: ty.clone(),
                                    };
                                    changed = true;
                                }
                            }
                        }
                        Instr::Copy { dest, src, ty } => {
                            if let Some(c) = consts.get(src).cloned() {
                                consts.insert(*dest, c.clone());
                                *in_ = Instr::Const {
                                    dest: *dest,
                                    c,
                                    ty: ty.clone(),
                                };
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
}

fn fold_binop(op: BinOp, l: &Const, r: &Const, _ty: Ty) -> Option<Const> {
    use BinOp::*;
    match (op, l, r) {
        (Add, Const::Int(a), Const::Int(b)) => Some(Const::Int(a.wrapping_add(*b))),
        (Sub, Const::Int(a), Const::Int(b)) => Some(Const::Int(a.wrapping_sub(*b))),
        (Mul, Const::Int(a), Const::Int(b)) => Some(Const::Int(a.wrapping_mul(*b))),
        (Div, Const::Int(a), Const::Int(b)) if *b != 0 => {
            if *a == i64::MIN && *b == -1 {
                Some(Const::Int(i64::MIN))
            } else {
                Some(Const::Int(a / b))
            }
        }
        (Mod, Const::Int(a), Const::Int(b)) if *b != 0 => {
            if *a == i64::MIN && *b == -1 {
                Some(Const::Int(0))
            } else {
                Some(Const::Int(a % b))
            }
        }
        (Eq, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a == b)),
        (Ne, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a != b)),
        (Lt, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a < b)),
        (Gt, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a > b)),
        (Le, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a <= b)),
        (Ge, Const::Int(a), Const::Int(b)) => Some(Const::Bool(a >= b)),
        (And, Const::Bool(a), Const::Bool(b)) => Some(Const::Bool(*a && *b)),
        (Or, Const::Bool(a), Const::Bool(b)) => Some(Const::Bool(*a || *b)),
        (Add, Const::Float(a), Const::Float(b)) => Some(Const::Float(a + b)),
        (Sub, Const::Float(a), Const::Float(b)) => Some(Const::Float(a - b)),
        (Mul, Const::Float(a), Const::Float(b)) => Some(Const::Float(a * b)),
        (Div, Const::Float(a), Const::Float(b)) => Some(Const::Float(a / b)),
        (Eq, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a == b)),
        (Ne, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a != b)),
        (Lt, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a < b)),
        (Gt, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a > b)),
        (Le, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a <= b)),
        (Ge, Const::Float(a), Const::Float(b)) => Some(Const::Bool(a >= b)),
        (Eq, Const::Str(a), Const::Str(b)) => Some(Const::Bool(a == b)),
        (Ne, Const::Str(a), Const::Str(b)) => Some(Const::Bool(a != b)),
        (Eq, Const::Bool(a), Const::Bool(b)) => Some(Const::Bool(a == b)),
        (Ne, Const::Bool(a), Const::Bool(b)) => Some(Const::Bool(a != b)),
        _ => None,
    }
}

fn fold_unop(op: UnOp, c: &Const, _ty: Ty) -> Option<Const> {
    use UnOp::*;
    match (op, c) {
        (Neg, Const::Int(v)) => Some(Const::Int(v.wrapping_neg())),
        (Neg, Const::Float(v)) => Some(Const::Float(-v)),
        (Not, Const::Bool(v)) => Some(Const::Bool(!v)),
        _ => None,
    }
}

/// Dead code elimination: remove instructions whose result value is never
/// used, and basic blocks unreachable from the entry block. Side-effecting
/// instructions (calls, stores, panics, allocas that are stored to) are kept.
pub fn dce(m: &mut MirModule) {
    for f in &mut m.functions {
        // Reachable blocks.
        let mut reachable: std::collections::HashSet<BlockId> = std::collections::HashSet::new();
        let mut stack = vec![f.entry];
        while let Some(b) = stack.pop() {
            if !reachable.insert(b) {
                continue;
            }
            if let Some(bb) = f.blocks.iter().find(|x| x.id == b) {
                match &bb.term {
                    Some(Term::Goto(t)) => stack.push(*t),
                    Some(Term::Branch {
                        then_bb, else_bb, ..
                    }) => {
                        stack.push(*then_bb);
                        stack.push(*else_bb);
                    }
                    _ => {}
                }
            }
        }
        f.blocks.retain(|b| reachable.contains(&b.id));

        // Iterative DCE to a fixed point.
        loop {
            // used values
            let mut used: std::collections::HashSet<Value> = std::collections::HashSet::new();
            for b in &f.blocks {
                for in_ in &b.instrs {
                    collect_uses(in_, &mut used);
                }
                if let Some(t) = &b.term {
                    match t {
                        Term::Branch { cond, .. } => {
                            used.insert(*cond);
                        }
                        Term::Return(Some(v)) => {
                            used.insert(*v);
                        }
                        _ => {}
                    }
                }
            }
            let mut changed = false;
            for b in &mut f.blocks {
                let before = b.instrs.len();
                b.instrs.retain(|in_| {
                    if is_side_effecting(in_) {
                        return true;
                    }
                    match instr_dest(in_) {
                        None => true,
                        Some(d) => used.contains(&d),
                    }
                });
                if b.instrs.len() != before {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

fn collect_uses(in_: &Instr, used: &mut std::collections::HashSet<Value>) {
    match in_ {
        Instr::Const { .. } => {}
        Instr::BinOp { lhs, rhs, .. } => {
            used.insert(*lhs);
            used.insert(*rhs);
        }
        Instr::UnOp { operand, .. } => {
            used.insert(*operand);
        }
        Instr::Call { args, .. } => {
            for a in args {
                used.insert(*a);
            }
        }
        Instr::Copy { src, .. } => {
            used.insert(*src);
        }
        Instr::Alloca { .. } => {}
        Instr::Load { slot, .. } => {
            used.insert(*slot);
        }
        Instr::Store { slot, value, .. } => {
            used.insert(*slot);
            used.insert(*value);
        }
        Instr::Record { fields, .. } => {
            for fv in fields {
                used.insert(*fv);
            }
        }
        Instr::FieldGet { base, .. } => {
            used.insert(*base);
        }
        Instr::FieldSet { base, value, .. } => {
            used.insert(*base);
            used.insert(*value);
        }
        Instr::Panic { msg } => {
            used.insert(*msg);
        }
        Instr::ArrayLit { elems, .. } => {
            for ev in elems {
                used.insert(*ev);
            }
        }
        Instr::IndexGet { base, index, .. } => {
            used.insert(*base);
            used.insert(*index);
        }
        Instr::IndexSet {
            base, index, value, ..
        } => {
            used.insert(*base);
            used.insert(*index);
            used.insert(*value);
        }
    }
}

fn instr_dest(in_: &Instr) -> Option<Value> {
    match in_ {
        Instr::Const { dest, .. }
        | Instr::BinOp { dest, .. }
        | Instr::UnOp { dest, .. }
        | Instr::Copy { dest, .. }
        | Instr::Alloca { dest, .. }
        | Instr::Load { dest, .. }
        | Instr::Record { dest, .. }
        | Instr::FieldGet { dest, .. }
        | Instr::FieldSet { dest, .. }
        | Instr::ArrayLit { dest, .. }
        | Instr::IndexGet { dest, .. }
        | Instr::IndexSet { dest, .. } => Some(*dest),
        Instr::Call { dest, .. } => *dest,
        _ => None,
    }
}

fn instr_ty(in_: &Instr) -> Option<Ty> {
    match in_ {
        Instr::Const { ty, .. }
        | Instr::BinOp { ty, .. }
        | Instr::UnOp { ty, .. }
        | Instr::Copy { ty, .. }
        | Instr::Alloca { ty, .. }
        | Instr::Load { ty, .. }
        | Instr::Store { ty, .. }
        | Instr::FieldGet { ty, .. }
        | Instr::FieldSet { ty, .. }
        | Instr::ArrayLit { ty, .. }
        | Instr::IndexGet { ty, .. }
        | Instr::IndexSet { ty, .. } => Some(ty.clone()),
        Instr::Call { ty, .. } => Some(ty.clone()),
        _ => None,
    }
}

fn is_side_effecting(in_: &Instr) -> bool {
    matches!(
        in_,
        Instr::Call { .. } | Instr::Store { .. } | Instr::Panic { .. }
    )
}

/// Inlining: inline small, pure, single-block functions (no allocas, stores,
/// loads, calls, or panics) at call sites. Repeats to a fixed point.
pub fn inline_small(m: &mut MirModule) {
    loop {
        if !try_inline_one(m) {
            break;
        }
    }
}

fn inlinable(g: &MirFn) -> bool {
    if g.blocks.len() != 1 {
        return false;
    }
    if g.mut_params.iter().any(|x| *x) {
        return false;
    }
    let b = &g.blocks[0];
    // last terminator must be Return(Some(_))
    match &b.term {
        Some(Term::Return(Some(_))) => {}
        _ => return false,
    }
    if b.instrs.len() > 8 {
        return false;
    }
    for in_ in &b.instrs {
        match in_ {
            Instr::Alloca { .. }
            | Instr::Load { .. }
            | Instr::Store { .. }
            | Instr::Panic { .. }
            | Instr::Call { .. } => return false,
            _ => {}
        }
    }
    true
}

fn try_inline_one(m: &mut MirModule) -> bool {
    // Find a call site.
    let mut site: Option<(usize, usize, usize, String, Vec<Value>, Value)> = None;
    'outer: for fi in 0..m.functions.len() {
        for bi in 0..m.functions[fi].blocks.len() {
            for ii in 0..m.functions[fi].blocks[bi].instrs.len() {
                if let Instr::Call {
                    dest: Some(d),
                    callee,
                    args,
                    ..
                } = &m.functions[fi].blocks[bi].instrs[ii]
                {
                    let cname = callee.clone();
                    let args = args.clone();
                    let d = *d;
                    // check callee exists and inlinable
                    if let Some(ci) = m.functions.iter().position(|f| f.name == cname) {
                        if ci != fi && inlinable(&m.functions[ci]) {
                            site = Some((fi, bi, ii, cname, args, d));
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    let (fi, bi, ii, cname, args, call_dest) = match site {
        Some(s) => s,
        None => return false,
    };

    // Clone callee data.
    let ci = m.functions.iter().position(|f| f.name == cname).unwrap();
    let g_block = m.functions[ci].blocks[0].clone();
    let g_params_count = m.functions[ci].params.len();
    let g_ret = m.functions[ci].ret.clone();

    // remap table
    let mut remap: HashMap<Value, Value> = HashMap::new();
    for i in 0..g_params_count {
        if i < args.len() {
            remap.insert(Value(i), args[i]);
        }
    }

    // fresh value counter
    let mut next = m.functions[fi]
        .value_types
        .keys()
        .copied()
        .max()
        .unwrap_or(0)
        + 1;

    // return value of callee
    let ret_val = match &g_block.term {
        Some(Term::Return(Some(v))) => v.0,
        _ => return false,
    };

    // Build replacement instrs.
    let mut new_instrs: Vec<Instr> = Vec::new();
    for in_ in &g_block.instrs {
        let mut in_ = in_.clone();
        // remap inputs
        remap_inputs(&mut in_, &remap);
        // remap dest
        if let Some(d) = instr_dest(&in_) {
            if d.0 == ret_val {
                set_instr_dest(&mut in_, call_dest);
                remap.insert(d, call_dest);
            } else {
                let fresh = Value(next);
                next += 1;
                set_instr_dest(&mut in_, fresh);
                remap.insert(d, fresh);
                // record type
                if let Some(t) = instr_ty(&in_) {
                    m.functions[fi].value_types.insert(fresh.0, t);
                }
            }
        }
        new_instrs.push(in_);
    }
    m.functions[fi].value_types.insert(call_dest.0, g_ret);

    // splice
    m.functions[fi].blocks[bi]
        .instrs
        .splice(ii..ii + 1, new_instrs);
    true
}

fn remap_inputs(in_: &mut Instr, remap: &HashMap<Value, Value>) {
    let r = |v: &mut Value| {
        if let Some(nv) = remap.get(v) {
            *v = *nv;
        }
    };
    match in_ {
        Instr::BinOp { lhs, rhs, .. } => {
            r(lhs);
            r(rhs);
        }
        Instr::UnOp { operand, .. } => {
            r(operand);
        }
        Instr::Call { args, .. } => {
            for a in args {
                r(a);
            }
        }
        Instr::Copy { src, .. } => {
            r(src);
        }
        Instr::Load { slot, .. } => {
            r(slot);
        }
        Instr::Store { slot, value, .. } => {
            r(slot);
            r(value);
        }
        Instr::Record { fields, .. } => {
            for fv in fields {
                r(fv);
            }
        }
        Instr::FieldGet { base, .. } => {
            r(base);
        }
        Instr::FieldSet { base, value, .. } => {
            r(base);
            r(value);
        }
        Instr::Panic { msg } => {
            r(msg);
        }
        Instr::ArrayLit { elems, .. } => {
            for ev in elems {
                r(ev);
            }
        }
        Instr::IndexGet { base, index, .. } => {
            r(base);
            r(index);
        }
        Instr::IndexSet {
            base, index, value, ..
        } => {
            r(base);
            r(index);
            r(value);
        }
        _ => {}
    }
}

fn set_instr_dest(in_: &mut Instr, v: Value) {
    match in_ {
        Instr::Const { dest, .. }
        | Instr::BinOp { dest, .. }
        | Instr::UnOp { dest, .. }
        | Instr::Copy { dest, .. }
        | Instr::Alloca { dest, .. }
        | Instr::Load { dest, .. }
        | Instr::Record { dest, .. }
        | Instr::FieldGet { dest, .. }
        | Instr::FieldSet { dest, .. }
        | Instr::ArrayLit { dest, .. }
        | Instr::IndexGet { dest, .. }
        | Instr::IndexSet { dest, .. } => *dest = v,
        Instr::Call { dest, .. } => *dest = Some(v),
        _ => {}
    }
}

/// Basic escape analysis: report which `concat` allocations escape their
/// defining function (returned or passed to a call). Non-escaping
/// allocations are candidates for future stack allocation / scalar
/// replacement.
pub fn escape_analysis(m: &MirModule) -> String {
    let mut out = String::new();
    out.push_str("# Escape analysis report\n\n");
    for f in &m.functions {
        // collect concat call dests
        let mut allocs: Vec<Value> = Vec::new();
        for b in &f.blocks {
            for in_ in &b.instrs {
                if let Instr::Call {
                    dest: Some(d),
                    callee,
                    ..
                } = in_
                {
                    if callee == "concat" {
                        allocs.push(*d);
                    }
                }
            }
        }
        if allocs.is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n", f.name));
        for a in &allocs {
            let escapes = allocation_escapes(f, *a);
            out.push_str(&format!(
                "  concat result %{}: {}\n",
                a.0,
                if escapes { "ESCAPES" } else { "local" }
            ));
        }
    }
    out
}

fn allocation_escapes(f: &MirFn, v: Value) -> bool {
    // An allocation escapes if its value is returned, passed to a call, or
    // stored into a slot that is itself returned/passed (conservative: any
    // store counts as escape).
    for b in &f.blocks {
        for in_ in &b.instrs {
            match in_ {
                Instr::Call { args, .. } => {
                    if args.contains(&v) {
                        return true;
                    }
                }
                Instr::Store { value, .. } if *value == v => {
                    return true;
                }
                _ => {}
            }
        }
        if let Some(Term::Return(Some(rv))) = &b.term {
            if *rv == v {
                return true;
            }
        }
    }
    false
}

/// Run the default (level 1) optimization pipeline.
pub fn optimize(m: &mut MirModule) {
    optimize_level(m, 1);
}

/// Run the optimization pipeline at the given level.
/// Level 0: no passes. Level 1: const fold + DCE + inlining.
pub fn optimize_level(m: &mut MirModule, level: u8) {
    if level == 0 {
        return;
    }
    const_fold(m);
    dce(m);
    inline_small(m);
    // Re-run const fold + dce after inlining.
    const_fold(m);
    dce(m);
}
