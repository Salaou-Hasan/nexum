//! Nexum HIR: name-free, typed, resolved high-level IR.
//!
//! See `docs/architecture/hir.md` for the design. The rules here:
//!
//! - Bindings are [`Slot`]s, never names. Declared entities are IDs.
//! - Every expression carries `ty: HTy`, always present.
//! - Every node that meant a decision carries the decided rule, so no
//!   later stage re-derives operator matrices, string semantics, method
//!   resolution, or copy discipline.
//! - The only `String`s anywhere are literals (`Str` values), the intern
//!   table, and the inert [`DiagInfo`] sidecar. That is a property of
//!   these type definitions -- readable, not mechanically enforced -- and
//!   the strip test in `crate::verify` proves the consequence that
//!   matters: deleting every `DiagInfo` name changes no verdict.
//! - HIR nodes are plain data. Malformed HIR is representable on purpose:
//!   the verifier (`crate::verify`) is the discipline, and its negative
//!   tests need something to reject.

pub use nx_ast::Span;

/// A binding in one function: parameters `0..n` in declaration order,
/// then every other binding in source order of first binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot(pub u32);

/// A module in [`HProgram::modules`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

/// A record type in [`HProgram::types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(pub u32);

/// A method in [`HProgram::methods`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MethodId(pub u32);

/// A function (or associated function, or method body) in
/// [`HProgram::funcs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// A module-level variable in [`HProgram::globals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalId(pub u32);

/// A field offset into a record layout. Constant by the time HIR
/// exists: the checker proved which field of which layout is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldIdx(pub usize);

/// An interned string in [`HProgram::strings`]: a field name for
/// dynamic field access. Interning (not inlining) keeps the name-freedom
/// test meaningful -- runtime names are deduplicated data, never
/// identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrId(pub u32);

/// Which field a field access means: a constant layout offset, or a
/// runtime-resolved name for a statically unknown base. The checker
/// resolves field names on known records; only `Unknown` bases stay
/// dynamic, and the verifier enforces exactly that split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldRef {
    Static(FieldIdx),
    Dynamic(StrId),
}

/// HIR value type. Mirrors the checker's `Ty`, except nominal references
/// are IDs: a `Record` names no names. `Func` and module-typed values can
/// never appear on a value node: functions are called, never referenced,
/// and a bare module in value position (`n = mm`) is rejected, so
/// conversion fails loudly on both rather than mistyping them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HTy {
    Int,
    Float,
    Bool,
    Str,
    None,
    Unknown,
    List(Box<HTy>),
    Dict(Box<HTy>),
    Record(TypeId),
}

impl std::fmt::Display for HTy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HTy::Int => write!(f, "Int"),
            HTy::Float => write!(f, "Float"),
            HTy::Bool => write!(f, "Bool"),
            HTy::Str => write!(f, "Str"),
            HTy::None => write!(f, "None"),
            HTy::Unknown => write!(f, "Unknown"),
            HTy::List(t) => write!(f, "List({t})"),
            HTy::Dict(t) => write!(f, "Dict({t})"),
            HTy::Record(id) => write!(f, "Record({})", id.0),
        }
    }
}

/// Inert original spellings: variable, type, method, field and module
/// names, for diagnostics and `dump-hir` readability only. Never
/// consulted for any decision; the strip test in `crate::verify` proves
/// it by deleting every one and re-verifying.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagInfo {
    pub name: Option<String>,
}

impl DiagInfo {
    pub fn named(name: &str) -> Self {
        DiagInfo {
            name: Some(name.to_string()),
        }
    }
}

/// What the backend emits for one arithmetic node, decided from the
/// recorded rule alone.
///
/// This table is the whole of the backend's arithmetic knowledge. It
/// takes no types, which is the point: a consumer cannot consult a type
/// to override a rule, because there is no type here to consult. The
/// `Trap` proof test in `nx-codegen` asserts that by feeding rules that
/// disagree with the operand types and checking which one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithPlan {
    /// A checked 64-bit intrinsic plus the trap it raises on overflow.
    Checked(&'static str),
    /// A raw floating-point instruction.
    FloatMnem(&'static str),
    /// A runtime helper call on `i64`.
    IntCall(&'static str),
    /// A runtime helper call on `double`.
    FloatCall(&'static str),
    /// A raw integer bitwise instruction.
    Bitwise(&'static str),
    /// The rule is dynamic: no unboxed form exists, so the caller
    /// dispatches on tags at runtime.
    Dispatch,
}

/// The emission plan for `rule` on `op`.
///
/// Note what is *not* a parameter: the operand types. Whoever holds a
/// rule holds the decision, and a consumer that wanted a different
/// answer would have to disagree with HIR loudly rather than quietly
/// recompute. `Dispatch` is the honest answer for a dynamic rule; every
/// other arm is a definite instruction or helper.
pub fn arith_plan(rule: BinRule, op: nx_ast::BinOp) -> ArithPlan {
    // Fully qualified rather than glob-imported: `Float` and `Bitwise`
    // each name both a `BinRule` variant and an `ArithPlan` variant.
    use nx_ast::BinOp as Op;
    use ArithPlan::{Bitwise as Plan, Checked, Dispatch, FloatCall, FloatMnem, IntCall};
    match rule {
        BinRule::Arith(ArithRule::Trap) => match op {
            Op::Add => Checked("llvm.sadd.with.overflow.i64"),
            Op::Sub => Checked("llvm.ssub.with.overflow.i64"),
            Op::Mul => Checked("llvm.smul.with.overflow.i64"),
            // Division and remainder keep the runtime's zero-divisor
            // panic; there is no checked intrinsic for either.
            Op::Div => IntCall("nx_div_i64"),
            Op::FloorDiv => IntCall("nx_floordiv_i64"),
            Op::Mod => IntCall("nx_mod_i64"),
            _ => Dispatch,
        },
        // Float and PromoteFloat agree on the instruction: the promotion
        // itself happens when the operands are coerced, which is the
        // caller's job, and both rules then compute in double.
        BinRule::Arith(ArithRule::Float) | BinRule::Arith(ArithRule::PromoteFloat) => match op {
            Op::Add => FloatMnem("fadd"),
            Op::Sub => FloatMnem("fsub"),
            Op::Mul => FloatMnem("fmul"),
            Op::Div => FloatCall("nx_fdiv"),
            Op::Pow => FloatCall("nx_fpow"),
            _ => Dispatch,
        },
        // R2: integer power saturates rather than wrapping, so it calls
        // the runtime helper that does the saturating; there is no
        // saturating intrinsic to inline.
        BinRule::Pow(PowRule::Saturate) => match op {
            Op::Pow => IntCall("nx_ipow"),
            _ => Dispatch,
        },
        BinRule::Bitwise => match op {
            Op::BitAnd => Plan("and"),
            Op::BitOr => Plan("or"),
            Op::BitXor => Plan("xor"),
            _ => Dispatch,
        },
        // A dynamic rule: at least one operand is statically unknown, so
        // there is no single instruction to emit.
        BinRule::Arith(ArithRule::Dynamic) | BinRule::Pow(PowRule::Dynamic) | BinRule::Dynamic => {
            Dispatch
        }
        // Unreachable through checked code: string concatenation has its
        // own node. Answering `Dispatch` rather than panicking keeps a
        // malformed node from taking the compiler down mid-emission.
        BinRule::Concat => Dispatch,
    }
}

/// Integer arithmetic rule (R1): trapping is the only defined behavior
/// for `Int` operands. The backend emits the checked intrinsic on `Trap`
/// unconditionally -- it never re-derives this from operand types.
/// `Dynamic` means at least one operand is statically unknown and the
/// runtime dispatches; it is rejected wherever all operands are known
/// (that would be a lowering bug, not a dynamic program).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithRule {
    /// `+ - * / // %` (binary and unary) on `Int`s: trap on overflow.
    /// `/` traps only on `MIN / -1`, the one unrepresentable quotient.
    Trap,
    /// Both operands `Float`.
    Float,
    /// One `Int`, one `Float`: promote and compute in `Float`.
    PromoteFloat,
    Dynamic,
}

/// Integer power rule (R2): saturation, never a trap on magnitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowRule {
    Saturate,
    Dynamic,
}

/// Decided rule for a binary operator that is not plain arithmetic.
/// `Dynamic` covers unknown operands on every arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinRule {
    Arith(ArithRule),
    Pow(PowRule),
    /// `& | ^ << >>`: integer-only by checker rule, no runtime decision.
    Bitwise,
    /// `+` on two strings: concatenation, no overflow or promotion rule.
    Concat,
    Dynamic,
}

/// Unary operator rule. `Not`/`BitNot`/`Pos` are total on their checker-
/// enforced operand types (the runtime helpers panic otherwise), so they
/// carry no decision; `Neg` carries R1 and `Dynamic` covers unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryRule {
    Neg(ArithRule),
    Not,
    BitNot,
    Pos,
    Dynamic,
}

/// Equality relation, decided from operand types. Containers and
/// records compare structurally (`nx_eq` dispatches on tags); `None`
/// only ever meets `None` or dynamic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqRule {
    Numeric,
    StrEq,
    Structural,
    IdentityNone,
    Dynamic,
}

/// Ordering relation, decided from operand types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpRule {
    Numeric,
    StrOrder,
    Dynamic,
}

/// Membership test, decided from the right-hand type. Substring search
/// is byte-wise over well-formed UTF-8, which can only match at
/// character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRule {
    ListEq,
    StrSub,
    DictKey,
    Dynamic,
}

/// Indexing, decided from the base type. `StrChar` is the R3 decision:
/// character index, negative from the end, one-character result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexRule {
    ListInt,
    StrChar,
    DictKey,
    Dynamic,
}

/// Slicing, decided from the base type. Bounds are `Int`; step
/// positivity stays a runtime check (usually known only at runtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceRule {
    ListCopy,
    StrChars,
    Dynamic,
}

/// Iteration element rule, shared by `for` loops and comprehensions so
/// both spellings agree by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterRule {
    List,
    StrChars,
    DictKeys,
    Dynamic,
}

/// A range in value position is always an ascending-or-empty list.
/// The descending form exists only as a `for` header (lowered to
/// `ForRange`, whose direction stays dynamic like the backend today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRule {
    AscendingOrEmpty,
}

/// What binding a value does (value semantics, grammar §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRule {
    CopyScalar,
    ShareStr,
    DeepClone,
    /// Statically unknown: the runtime clones containers and shares the
    /// rest, exactly as `nx_clone` does today.
    Dynamic,
}

/// What `del` does, decided from target shape and container type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelRule {
    /// A name: unbind it. Later uses are statically undefined, so the
    /// backend's rebind-to-`None` is unobservable through checked code.
    Unbind,
    ListRemove,
    DictRemove,
    /// A record field: blank to `None` (arity is fixed).
    RecordBlank,
    Dynamic,
}

/// Ambient builtins. Arity is checked in lowering; `Push` targets a
/// place, `Input` takes an optional prompt, `ToInt`/`ToFloat` convert
/// one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinOp {
    Len,
    Push,
    Input,
    ToInt,
    ToFloat,
}

/// Method receiver kind, carried through from the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    Read,
    Mut,
    Own,
}

/// A readable place: a local slot or a module global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Slot(Slot),
    Global(GlobalId),
}

/// A writable target.
#[derive(Debug, Clone, PartialEq)]
pub enum HTarget {
    Slot(Slot),
    Global(GlobalId),
    Index {
        base: Box<HExpr>,
        index: Box<HExpr>,
        rule: IndexRule,
    },
    Field {
        base: Box<HExpr>,
        field: FieldRef,
    },
}

/// A typed expression: the type and the decided rule travel with the
/// node, so consumers match on answers, never on source spellings.
#[derive(Debug, Clone, PartialEq)]
pub struct HExpr {
    pub span: Span,
    pub diag: DiagInfo,
    pub ty: HTy,
    pub kind: HExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    None,
    List(Vec<HExpr>),
    Range {
        start: Box<HExpr>,
        end: Box<HExpr>,
        rule: RangeRule,
    },
    Dict(Vec<(HExpr, HExpr)>),
    /// A slot or global read.
    Place(Place),
    Field {
        base: Box<HExpr>,
        field: FieldRef,
    },
    Index {
        base: Box<HExpr>,
        index: Box<HExpr>,
        rule: IndexRule,
    },
    Slice {
        base: Box<HExpr>,
        from: Option<Box<HExpr>>,
        to: Option<Box<HExpr>>,
        step: Option<Box<HExpr>>,
        rule: SliceRule,
    },
    Unary {
        op: nx_ast::UnaryOp,
        rule: UnaryRule,
        operand: Box<HExpr>,
    },
    /// Arithmetic, power and bitwise operators. Comparisons, equality
    /// and membership are separate nodes below: they answer `Bool`
    /// through different relations, and sharing one rule field would
    /// let an ordering relation sit on an addition.
    Binary {
        left: Box<HExpr>,
        op: nx_ast::BinOp,
        rule: BinRule,
        right: Box<HExpr>,
    },
    Equal {
        left: Box<HExpr>,
        op: nx_ast::BinOp,
        rule: EqRule,
        right: Box<HExpr>,
    },
    Compare {
        left: Box<HExpr>,
        op: nx_ast::BinOp,
        rule: CmpRule,
        right: Box<HExpr>,
    },
    Contains {
        needle: Box<HExpr>,
        hay: Box<HExpr>,
        rule: MemberRule,
        negated: bool,
    },
    /// `a if c else b`, with the joined type on the node.
    Select {
        cond: Box<HExpr>,
        then_value: Box<HExpr>,
        else_value: Box<HExpr>,
    },
    /// Short-circuit `and` / `or`. Structure only; MIR builds the diamond.
    Logic {
        op: nx_ast::BinOp,
        left: Box<HExpr>,
        right: Box<HExpr>,
    },
    Compr {
        element: Box<HExpr>,
        var: Slot,
        iter: Box<HExpr>,
        rule: IterRule,
        cond: Option<Box<HExpr>>,
    },
    CallFn {
        func: FuncId,
        args: Vec<HExpr>,
    },
    /// A method call. `receiver` is `None` only for `T.m(...)` through a
    /// type name (types are not values, so nothing evaluates); a value
    /// base always evaluates, even for associated functions (its effects
    /// run, but it is not passed). `writeback` is `Some` target iff the
    /// method is `mut self` and the receiver has storage.
    CallMethod {
        method: MethodId,
        receiver: Option<Box<HExpr>>,
        args: Vec<HExpr>,
        writeback: Option<HTarget>,
    },
    Construct {
        type_id: TypeId,
        args: Vec<HExpr>,
    },
    Builtin {
        op: BuiltinOp,
        args: Vec<HExpr>,
    },
}

/// A statement with its source position and inert original spellings.
/// Control-flow bodies are plain blocks of these; see [`HStmtKind`].
#[derive(Debug, Clone, PartialEq)]
pub struct HStmt {
    pub span: Span,
    pub diag: DiagInfo,
    pub kind: HStmtKind,
}

/// Statement structure. Declarations and imports leave no nodes: they
/// become table entries, resolved references, and [`HStmtKind::EnsureInit`]
/// markers (see there).
#[derive(Debug, Clone, PartialEq)]
pub enum HStmtKind {
    /// Positional assignment; several targets against one value is
    /// destructuring, exactly as the AST means it. One rule per value:
    /// the rule is a property of how each value binds.
    Assign {
        targets: Vec<HTarget>,
        values: Vec<HExpr>,
        rules: Vec<CopyRule>,
    },
    /// Target-first, evaluated-once order is structural intent here;
    /// MIR makes the temporaries explicit.
    AssignOp {
        target: HTarget,
        op: nx_ast::BinOp,
        rule: BinRule,
        value: HExpr,
    },
    Print {
        values: Vec<HExpr>,
    },
    /// Ensure a module is initialized, at the position of the `import`
    /// or `from ... import` statement that needs it -- including inside
    /// function bodies, where the import executes when reached. Repeat
    /// initialization is a no-op downstream. Top-level imports produce
    /// these nodes too: initialization follows statement order, not
    /// module-table order.
    EnsureInit {
        module: ModuleId,
    },
    If {
        cond: HExpr,
        then_body: Vec<HStmt>,
        elifs: Vec<(HExpr, Vec<HStmt>)>,
        else_body: Option<Vec<HStmt>>,
    },
    While {
        cond: HExpr,
        body: Vec<HStmt>,
    },
    /// Counted loop over a range. Direction stays dynamic (the backend
    /// emits the up/down select as today); bounds are `Int`.
    ForRange {
        var: Slot,
        start: HExpr,
        end: HExpr,
        body: Vec<HStmt>,
    },
    ForEach {
        var: Slot,
        iter: HExpr,
        rule: IterRule,
        body: Vec<HStmt>,
    },
    Return {
        values: Vec<HExpr>,
    },
    Break,
    Continue,
    Del {
        targets: Vec<HDelTarget>,
    },
    Assert {
        cond: HExpr,
        message: Option<HExpr>,
    },
    Expr(HExpr),
}

/// A block: statements in order.
pub type Block = Vec<HStmt>;

/// A deletion with its decided rule.
#[derive(Debug, Clone, PartialEq)]
pub struct HDelTarget {
    pub target: HTarget,
    pub rule: DelRule,
}

/// A function body with its signature. Methods are entries here too;
/// [`HMethod`] points at the entry and records whose method it is.
#[derive(Debug, Clone, PartialEq)]
pub struct HFunc {
    pub params: Vec<(Slot, HTy)>,
    pub ret: HTy,
    pub body: Block,
    pub diag: DiagInfo,
}

/// A method: which type, which function body, which receiver.
/// Associated functions (`None`) live here too, so every `T.m` spelling
/// resolves through one table; they take no receiver at calls.
#[derive(Debug, Clone, PartialEq)]
pub struct HMethod {
    pub type_id: TypeId,
    pub func: FuncId,
    pub receiver: Option<ReceiverKind>,
    pub diag: DiagInfo,
}

/// A record layout: field types in declaration order. Recursive layouts
/// work because fields reference [`TypeId`], resolved after all
/// declarations are indexed.
#[derive(Debug, Clone, PartialEq)]
pub struct HType {
    pub module: ModuleId,
    pub fields: Vec<HTy>,
    pub diag: DiagInfo,
}

/// A module-level variable.
#[derive(Debug, Clone, PartialEq)]
pub struct HGlobal {
    pub module: ModuleId,
    pub diag: DiagInfo,
}

/// A module: its share of the program tables plus its top-level code as
/// an ordinary function (the `<top>` convention, kept).
#[derive(Debug, Clone, PartialEq)]
pub struct HModule {
    pub diag: DiagInfo,
    pub globals: Vec<GlobalId>,
    pub types: Vec<TypeId>,
    pub funcs: Vec<FuncId>,
    pub top: FuncId,
}

/// A whole lowered program.
#[derive(Debug, Clone, PartialEq)]
pub struct HProgram {
    pub modules: Vec<HModule>,
    pub types: Vec<HType>,
    pub methods: Vec<HMethod>,
    pub funcs: Vec<HFunc>,
    pub globals: Vec<HGlobal>,
    /// Interned runtime strings (dynamic field names). No other
    /// `String` in HIR holds a source identifier; see `DiagInfo`.
    pub strings: Vec<String>,
    pub entry: ModuleId,
}
