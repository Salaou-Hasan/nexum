//! AST for Nexum.
//!
//! Two shapes here are load-bearing and easy to get wrong when reading
//! further down the compiler:
//!
//! - [`Target`] is what an assignment can write to. It is deliberately a
//!   separate type from [`Expr`]: `a[i] = v` and `p.x = v` are assignments,
//!   not calls that happen to return the container.
//! - [`BinOp::Mod`] follows Python's sign convention, the result takes the
//!   sign of the divisor, so `-7 % 3` is `2`. The runtime has to match.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// One or more targets and one or more values. A single pair is the
    /// ordinary `a = 1`; several is either multiple assignment
    /// (`a, b = 1, 2`) or destructuring of a tuple return (`a, b = f()`).
    Assign { targets: Vec<Target>, values: Vec<Expr>, span: Span },
    /// `target op= value`, for a target that can be both read and written.
    AssignOp { target: Target, op: BinOp, value: Expr, span: Span },
    Print { values: Vec<Expr>, span: Span },
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        elifs: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    For {
        var: String,
        iter: ForIter,
        body: Vec<Stmt>,
        span: Span,
    },
    Fn {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        span: Span,
    },
    Return {
        /// More than one value means a tuple return, destructured by the
        /// caller with multiple assignment.
        values: Vec<Expr>,
        span: Span,
    },
    Break { span: Span },
    Continue { span: Span },
    Parallel { tasks: Vec<Stmt>, span: Span },
    Import { module: String, alias: Option<String>, span: Span },
    FromImport { module: String, names: Vec<(String, Option<String>)>, span: Span },
    /// `del a`, `del a[i]`, `del p.x`
    Del { targets: Vec<Target>, span: Span },
    /// `assert cond` / `assert cond, "message"`
    Assert { cond: Expr, message: Option<Expr>, span: Span },
    Expr(Expr),
}

/// Somewhere an assignment can write. Reading one yields an [`Expr`], so
/// the checker and codegen have one path for "read this name" and one for
/// "write here".
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A plain binding. The entry is the name.
    Name(String),
    /// `base[index]`
    Index { base: Box<Expr>, index: Box<Expr> },
    /// `base.field`
    Attr { base: Box<Expr>, field: String },
}

impl Target {
    pub fn span(&self) -> Span {
        match self {
            Target::Name(_) => Span { line: 0, col: 0 },
            Target::Index { base, .. } => base.span(),
            Target::Attr { base, .. } => base.span(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64, Span),
    Float(f64, Span),
    Bool(bool, Span),
    Str(String, Span),
    /// The unit value, written `None`.
    NoneLit(Span),
    List(Vec<Expr>, Span),
    /// `start..end` -- half-open, ascending. An expression rather than
    /// `for`-only syntax so a range can be iterated, sliced and passed
    /// like any other list, which is what makes it useful as a
    /// comprehension's source (`[i * i for i in 0..5]`).
    Range { start: Box<Expr>, end: Box<Expr>, span: Span },
    /// `{k: v, ...}`. Order is preserved so iteration is deterministic.
    Dict(Vec<(Expr, Expr)>, Span),
    Var(String, Span),
    Attr {
        base: Box<Expr>,
        attr: String,
        span: Span,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    /// `base[from:to:step]`. Absent bounds are `None`, so `a[:]`, `a[1:]`,
    /// `a[:3]` and `a[::2]` all parse through this one form.
    Slice {
        base: Box<Expr>,
        from: Option<Box<Expr>>,
        to: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
        span: Span,
    },
    /// `value if cond else other` -- an expression, so it composes.
    IfExpr {
        cond: Box<Expr>,
        then_value: Box<Expr>,
        else_value: Box<Expr>,
        span: Span,
    },
    /// `[expr for var in iter if cond]`. The `if` is optional and there is
    /// at most one, which is all that is needed to be useful.
    Comprehension {
        element: Box<Expr>,
        var: String,
        iter: Box<Expr>,
        cond: Option<Box<Expr>>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForIter {
    Range { start: Expr, end: Expr },
    Each(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `//` -- floor division. Truncating division on two Ints already
    /// floors toward negative infinity, so this is a spelling difference
    /// rather than a semantic one; it exists because the floor is what
    /// people mean by "how many whole times".
    FloorDiv,
    /// `%` -- remainder, with Python's sign convention: the result takes
    /// the sign of the divisor, so `-7 % 3` is `2`. The runtime and the
    /// interpreter both have to agree on this.
    Mod,
    /// `**` -- exponentiation, right associative.
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    /// `x in y` -- membership in a list, a string's characters, or a dict's
    /// keys.
    In,
    /// `x not in y`
    NotIn,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::NotEq => "!=",
            BinOp::Lt => "<",
            BinOp::LtEq => "<=",
            BinOp::Gt => ">",
            BinOp::GtEq => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
        }
    }

    /// The `op=` spelling, for augmented assignment. `None` for operators
    /// with no compound form: `and`, `or`, membership and the comparisons
    /// are not assignments in disguise.
    pub fn assign_sigil(self) -> Option<&'static str> {
        match self {
            BinOp::Add => Some("+="),
            BinOp::Sub => Some("-="),
            BinOp::Mul => Some("*="),
            BinOp::Div => Some("/="),
            BinOp::FloorDiv => Some("//="),
            BinOp::Mod => Some("%="),
            BinOp::Pow => Some("**="),
            BinOp::BitAnd => Some("&="),
            BinOp::BitOr => Some("|="),
            BinOp::BitXor => Some("^="),
            BinOp::Shl => Some("<<="),
            BinOp::Shr => Some(">>="),
            _ => None,
        }
    }

    /// Anything that yields a Bool. `in` and `not in` are comparisons for
    /// the purpose of the checker's operand rules.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::NotEq | BinOp::In | BinOp::NotIn
        )
    }

    /// The subset that orders rather than tests equality.
    pub fn is_ordering(self) -> bool {
        matches!(self, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
    }

    /// Integer-only operators. Applying one to a Float is a type error
    /// rather than a silent conversion, which is why this is separate from
    /// the arithmetic set.
    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    /// `~x` -- bitwise complement
    BitNot,
    /// Unary `+`. Exists so that `+x` round-trips through code generated
    /// from an expression tree, and reads as a no-op to a human.
    Pos,
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "not",
            UnaryOp::BitNot => "~",
            UnaryOp::Pos => "+",
        }
    }
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Float(_, s)
            | Expr::Bool(_, s)
            | Expr::Str(_, s)
            | Expr::NoneLit(s)
            | Expr::List(_, s)
            | Expr::Dict(_, s)
            | Expr::Var(_, s) => *s,
            Expr::Range { span, .. }
            | Expr::Attr { span, .. }
            | Expr::Index { span, .. }
            | Expr::Slice { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::IfExpr { span, .. }
            | Expr::Comprehension { span, .. }
            | Expr::Call { span, .. } => *span,
        }
    }
}

impl Target {
    /// The expression that reads whatever this target names. Lets the
    /// checker and codegen handle `x += 1` and `a[i] += 1` through one path.
    pub fn as_expr(&self, span: Span) -> Expr {
        match self {
            Target::Name(n) => Expr::Var(n.clone(), span),
            Target::Index { base, index } => {
                Expr::Index { base: base.clone(), index: index.clone(), span }
            }
            Target::Attr { base, field } => {
                Expr::Attr { base: base.clone(), attr: field.clone(), span }
            }
        }
    }
}
