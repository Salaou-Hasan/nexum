//! Parser and AST for Nexum (bootstrap). AST carries source spans on every node.

use nexum_diagnostics::Diagnostic;
use nexum_lexer::{lex, Token, TokenKind};
use nexum_source::{SourceMap, Span};

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub mod normalize;

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String),
    Array(Box<TypeExpr>, usize),
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
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    Call {
        callee: String,
        args: Vec<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Array(Vec<Expr>),
}

/// One step of an assignment target: `a.b[i].c = v` is a root `a` with
/// segments `Field("b")`, `Index(i)`, `Field("c")`.
#[derive(Debug, Clone)]
pub enum PathSeg {
    Field(String),
    Index(Expr),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    /// `mut` marks a by-reference (borrowing) parameter: the callee can assign
    /// through it and the caller observes the change.
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Fn {
        name: String,
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        body: Vec<Stmt>,
    },
    Let {
        name: String,
        mutable: bool,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        name: String,
        value: Expr,
        /// `Some(op)` for compound assignment (`x += e` ≡ `x = x + e`).
        op: Option<BinOp>,
    },
    PathAssign {
        root: String,
        path: Vec<PathSeg>,
        value: Expr,
        /// `Some(op)` for compound assignment (`xs[i] += e`).
        op: Option<BinOp>,
    },
    TypeDecl {
        name: String,
        fields: Vec<(String, TypeExpr)>,
    },
    If {
        cond: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: ForIter,
        body: Vec<Stmt>,
    },
    Return {
        value: Option<Expr>,
    },
    Expr(Expr),
    Import {
        path: String,
    },
}

/// How a `for` statement iterates.
#[derive(Debug, Clone)]
pub enum ForIter {
    /// `for i in lo..hi:` — an integer range, `lo` inclusive, `hi` exclusive.
    Range(Expr, Expr),
    /// `for v in xs:` — each element of an array, by value.
    Each(Expr),
}

#[derive(Debug, Clone)]
pub struct Module {
    pub stmts: Vec<Stmt>,
}

pub struct ParseResult {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(tokens: &[Token]) -> ParseResult {
    let mut p = Parser {
        tokens,
        i: 0,
        diagnostics: Vec::new(),
        depth: 0,
    };
    let stmts = p.parse_module();
    ParseResult {
        module: Module { stmts },
        diagnostics: p.diagnostics,
    }
}

/// Load a source file and everything it imports into `sm`, returning a single
/// flattened module.
///
/// Every file is added to the shared [`SourceMap`] exactly once, so diagnostics
/// and spans always refer to the right file. Imports are resolved relative to
/// the directory of the importing file. A file that is reached twice (diamond
/// import) or recursively (import cycle) is loaded only once; cycles are
/// therefore broken by construction rather than by a separate check.
pub fn load_module(sm: &mut SourceMap, root: &Path) -> (Module, Vec<Diagnostic>) {
    let mut loaded: HashSet<PathBuf> = HashSet::new();
    let mut stmts: Vec<Stmt> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();
    load_file(sm, root, &mut loaded, &mut stmts, &mut diags);
    (Module { stmts }, diags)
}

fn load_file(
    sm: &mut SourceMap,
    path: &Path,
    loaded: &mut HashSet<PathBuf>,
    stmts: &mut Vec<Stmt>,
    diags: &mut Vec<Diagnostic>,
) {
    // Canonicalize so that `a.nx` and `./a.nx` are the same file, and so a
    // cycle terminates.
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !loaded.insert(key) {
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            diags.push(Diagnostic::error(
                "T047",
                format!("cannot read `{}`: {e}", path.display()),
            ));
            return;
        }
    };
    let fid = sm.add(path.to_path_buf(), text);
    let sf = sm.get(fid);
    let lexed = lex(sf);
    let lex_errors = lexed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error);
    diags.extend(lexed.diagnostics.iter().cloned());
    if lex_errors {
        return;
    }
    let parsed = parse(&lexed.tokens);
    let parse_errors = parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error);
    diags.extend(parsed.diagnostics.iter().cloned());
    if parse_errors {
        return;
    }
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for s in &parsed.module.stmts {
        match &s.kind {
            StmtKind::Import { path: rel } => {
                let target = base.join(rel);
                if !target.exists() {
                    diags.push(
                        Diagnostic::error(
                            "T047",
                            format!("cannot import `{}`: file not found", rel),
                        )
                        .with_span(Span::new(
                            fid,
                            s.span.start,
                            s.span.start,
                        )),
                    );
                    continue;
                }
                load_file(sm, &target, loaded, stmts, diags);
            }
            _ => stmts.push(s.clone()),
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    i: usize,
    diagnostics: Vec<Diagnostic>,
    /// Nesting depth: 0 at module level, >0 inside a function body.
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.i.min(self.tokens.len() - 1)]
    }
    fn kind(&self) -> TokenKind {
        self.peek().kind
    }
    fn advance(&mut self) {
        if self.i < self.tokens.len() {
            self.i += 1;
        }
    }
    fn prev_span_end(&self) -> usize {
        if self.i == 0 {
            0
        } else {
            self.tokens[self.i - 1].span.end
        }
    }
    fn expect(&mut self, k: TokenKind) -> bool {
        if self.kind() == k {
            self.advance();
            true
        } else {
            self.diagnostics.push(
                Diagnostic::error("P001", format!("expected {:?}, found {:?}", k, self.kind()))
                    .with_span(self.peek().span),
            );
            false
        }
    }
    fn skip_newlines(&mut self) {
        while self.kind() == TokenKind::Newline {
            self.advance();
        }
    }

    fn parse_module(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while self.kind() != TokenKind::Eof {
            if self.kind() == TokenKind::Dedent {
                self.advance();
                continue;
            }
            if let Some(s) = self.parse_stmt() {
                stmts.push(s);
            } else {
                self.advance();
            }
            self.skip_newlines();
        }
        stmts
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let start = self.peek().span.start;
        match self.kind() {
            TokenKind::KwFn => self.parse_fn(start),
            TokenKind::KwType => self.parse_type_decl(start),
            TokenKind::KwLet => self.parse_let(start),
            TokenKind::KwIf => self.parse_if(start),
            TokenKind::KwWhile => self.parse_while(start),
            TokenKind::KwFor => self.parse_for(start),
            TokenKind::KwReturn => self.parse_return(start),
            TokenKind::KwImport => self.parse_import(start),
            TokenKind::Ident => {
                // Lookahead for a path assignment `a.b[i].c = ...`: scan the
                // lvalue shape (identifier followed by `.ident` / `[...]`
                // segments) up to a top-level `=`. Bracket depth is tracked so
                // commas and operators inside `[...]` do not confuse the scan.
                let mut j = self.i + 1;
                let mut depth: usize = 0;
                let mut is_path = true;
                // None: no top-level assignment token seen yet.
                // Some(None): plain `=`; Some(Some(op)): compound `op=`.
                let mut found_eq: Option<Option<BinOp>> = None;
                while j < self.tokens.len() {
                    match self.tokens[j].kind {
                        TokenKind::Dot if depth == 0 => {
                            if j + 1 < self.tokens.len()
                                && self.tokens[j + 1].kind == TokenKind::Ident
                            {
                                j += 2;
                            } else {
                                is_path = false;
                                break;
                            }
                        }
                        TokenKind::LBracket => {
                            depth += 1;
                            j += 1;
                        }
                        TokenKind::RBracket => {
                            if depth == 0 {
                                is_path = false;
                                break;
                            }
                            depth -= 1;
                            j += 1;
                        }
                        TokenKind::Eq if depth == 0 => {
                            found_eq = Some(None);
                            break;
                        }
                        TokenKind::PlusEq
                        | TokenKind::MinusEq
                        | TokenKind::StarEq
                        | TokenKind::SlashEq
                        | TokenKind::PercentEq
                            if depth == 0 =>
                        {
                            found_eq = Some(Some(match self.tokens[j].kind {
                                TokenKind::PlusEq => BinOp::Add,
                                TokenKind::MinusEq => BinOp::Sub,
                                TokenKind::StarEq => BinOp::Mul,
                                TokenKind::SlashEq => BinOp::Div,
                                _ => BinOp::Mod,
                            }));
                            break;
                        }
                        TokenKind::Newline
                        | TokenKind::Eof
                        | TokenKind::Indent
                        | TokenKind::Dedent
                            if depth == 0 =>
                        {
                            break;
                        }
                        _ => j += 1,
                    }
                }
                if is_path && found_eq.is_some() {
                    let root = self.peek().text.clone();
                    self.advance();
                    let mut path: Vec<PathSeg> = Vec::new();
                    loop {
                        match self.kind() {
                            TokenKind::Dot => {
                                self.advance();
                                if self.kind() != TokenKind::Ident {
                                    self.diagnostics.push(
                                        Diagnostic::error("P006", "expected field name after `.`")
                                            .with_span(self.peek().span),
                                    );
                                    return None;
                                }
                                path.push(PathSeg::Field(self.peek().text.clone()));
                                self.advance();
                            }
                            TokenKind::LBracket => {
                                self.advance();
                                let index = self.parse_expr();
                                if self.kind() != TokenKind::RBracket {
                                    self.diagnostics.push(
                                        Diagnostic::error("P010", "expected `]` after index")
                                            .with_span(self.peek().span),
                                    );
                                    return None;
                                }
                                self.advance();
                                path.push(PathSeg::Index(index));
                            }
                            TokenKind::Eq
                            | TokenKind::PlusEq
                            | TokenKind::MinusEq
                            | TokenKind::StarEq
                            | TokenKind::SlashEq
                            | TokenKind::PercentEq => break,
                            _ => {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        "P011",
                                        "expected `=`, `.field` or `[index]` in assignment target",
                                    )
                                    .with_span(self.peek().span),
                                );
                                return None;
                            }
                        }
                    }
                    let op = match self.kind() {
                        TokenKind::Eq => None,
                        TokenKind::PlusEq => Some(BinOp::Add),
                        TokenKind::MinusEq => Some(BinOp::Sub),
                        TokenKind::StarEq => Some(BinOp::Mul),
                        TokenKind::SlashEq => Some(BinOp::Div),
                        _ => Some(BinOp::Mod),
                    };
                    self.advance(); // = / += / -= / *= / /= / %=
                    let value = self.parse_expr();
                    let span = Span::new(self.peek().span.file, start, self.prev_span_end());
                    let kind = if path.is_empty() {
                        StmtKind::Assign {
                            name: root,
                            value,
                            op,
                        }
                    } else {
                        StmtKind::PathAssign {
                            root,
                            path,
                            value,
                            op,
                        }
                    };
                    Some(Stmt { kind, span })
                } else {
                    let e = self.parse_expr();
                    let span = e.span;
                    Some(Stmt {
                        kind: StmtKind::Expr(e),
                        span,
                    })
                }
            }
            _ => {
                let e = self.parse_expr();
                let span = e.span;
                Some(Stmt {
                    kind: StmtKind::Expr(e),
                    span,
                })
            }
        }
    }

    fn parse_fn(&mut self, start: usize) -> Option<Stmt> {
        self.advance(); // fn
        if self.kind() != TokenKind::Ident {
            self.diagnostics.push(
                Diagnostic::error("P002", "expected function name").with_span(self.peek().span),
            );
            return None;
        }
        let name = self.peek().text.clone();
        self.advance();
        self.expect(TokenKind::LParen);
        let mut params = Vec::new();
        while self.kind() != TokenKind::RParen && self.kind() != TokenKind::Eof {
            let mutable = if self.kind() == TokenKind::KwMut {
                self.advance();
                true
            } else {
                false
            };
            if self.kind() == TokenKind::Ident {
                let pname = self.peek().text.clone();
                self.advance();
                let ty = if self.kind() == TokenKind::Colon {
                    self.advance();
                    self.parse_type_expr()
                } else {
                    None
                };
                params.push(Param {
                    name: pname,
                    ty,
                    mutable,
                });
            }
            if self.kind() == TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen);
        let ret = if self.kind() == TokenKind::Arrow {
            self.advance();
            self.parse_type_expr()
        } else {
            None
        };
        if self.kind() == TokenKind::Colon {
            self.advance();
        }
        let body = self.parse_block();
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::Fn {
                name,
                params,
                ret,
                body,
            },
            span,
        })
    }

    fn parse_type_decl(&mut self, start: usize) -> Option<Stmt> {
        self.advance(); // type
        if self.kind() != TokenKind::Ident {
            self.diagnostics
                .push(Diagnostic::error("P007", "expected type name").with_span(self.peek().span));
            return None;
        }
        let name = self.peek().text.clone();
        self.advance();
        if self.kind() == TokenKind::Colon {
            self.advance();
        }
        let mut fields: Vec<(String, TypeExpr)> = Vec::new();
        while self.kind() == TokenKind::Newline {
            self.advance();
        }
        if self.kind() == TokenKind::Indent {
            self.advance();
            while self.kind() != TokenKind::Dedent && self.kind() != TokenKind::Eof {
                self.skip_newlines();
                if self.kind() == TokenKind::Dedent || self.kind() == TokenKind::Eof {
                    break;
                }
                if self.kind() != TokenKind::Ident {
                    self.diagnostics.push(
                        Diagnostic::error("P008", "expected field name")
                            .with_span(self.peek().span),
                    );
                    while self.kind() != TokenKind::Newline
                        && self.kind() != TokenKind::Dedent
                        && self.kind() != TokenKind::Eof
                    {
                        self.advance();
                    }
                    continue;
                }
                let fname = self.peek().text.clone();
                self.advance();
                let fty = if self.kind() == TokenKind::Colon {
                    self.advance();
                    match self.parse_type_expr() {
                        Some(t) => t,
                        None => {
                            self.diagnostics.push(
                                Diagnostic::error("P009", "expected field type")
                                    .with_span(self.peek().span),
                            );
                            TypeExpr::Named("Int".to_string())
                        }
                    }
                } else {
                    self.diagnostics.push(
                        Diagnostic::error("P009", "expected `:` and field type")
                            .with_span(self.peek().span),
                    );
                    TypeExpr::Named("Int".to_string())
                };
                fields.push((fname, fty));
                // consume rest of line
                while self.kind() != TokenKind::Newline
                    && self.kind() != TokenKind::Dedent
                    && self.kind() != TokenKind::Eof
                {
                    self.advance();
                }
            }
            if self.kind() == TokenKind::Dedent {
                self.advance();
            }
        }
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::TypeDecl { name, fields },
            span,
        })
    }

    fn parse_type_expr(&mut self) -> Option<TypeExpr> {
        if self.kind() == TokenKind::Ident {
            let t = self.peek().text.clone();
            self.advance();
            Some(TypeExpr::Named(t))
        } else if self.kind() == TokenKind::LBracket {
            // Array type: `[T; N]`
            self.advance();
            let elem = self.parse_type_expr();
            if self.kind() != TokenKind::Semicolon {
                self.diagnostics.push(
                    Diagnostic::error("P012", "expected `;` and length in array type")
                        .with_span(self.peek().span),
                );
                return elem.map(|e| TypeExpr::Array(Box::new(e), 0));
            }
            self.advance();
            let n = if self.kind() == TokenKind::Int {
                let v = self.peek().text.parse::<usize>().unwrap_or(0);
                self.advance();
                v
            } else {
                self.diagnostics.push(
                    Diagnostic::error("P013", "expected array length").with_span(self.peek().span),
                );
                0
            };
            self.expect(TokenKind::RBracket);
            elem.map(|e| TypeExpr::Array(Box::new(e), n))
        } else {
            None
        }
    }

    fn parse_let(&mut self, start: usize) -> Option<Stmt> {
        self.advance();
        let mutable = if self.kind() == TokenKind::KwMut {
            self.advance();
            true
        } else {
            false
        };
        if self.kind() != TokenKind::Ident {
            self.diagnostics.push(
                Diagnostic::error("P003", "expected variable name").with_span(self.peek().span),
            );
            return None;
        }
        let name = self.peek().text.clone();
        self.advance();
        let ty = if self.kind() == TokenKind::Colon {
            self.advance();
            self.parse_type_expr()
        } else {
            None
        };
        self.expect(TokenKind::Eq);
        let value = self.parse_expr();
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::Let {
                name,
                mutable,
                ty,
                value,
            },
            span,
        })
    }

    fn parse_if(&mut self, start: usize) -> Option<Stmt> {
        self.advance();
        let cond = self.parse_expr();
        if self.kind() == TokenKind::Colon {
            self.advance();
        }
        let then_branch = self.parse_block();
        let else_branch = if self.kind() == TokenKind::KwElse {
            self.advance();
            if self.kind() == TokenKind::Colon {
                self.advance();
            }
            Some(self.parse_block())
        } else {
            None
        };
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch,
            },
            span,
        })
    }

    fn parse_while(&mut self, start: usize) -> Option<Stmt> {
        self.advance();
        let cond = self.parse_expr();
        if self.kind() == TokenKind::Colon {
            self.advance();
        }
        let body = self.parse_block();
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::While { cond, body },
            span,
        })
    }

    fn parse_for(&mut self, start: usize) -> Option<Stmt> {
        self.advance();
        if self.kind() != TokenKind::Ident {
            return None;
        }
        let var = self.peek().text.clone();
        self.advance();
        self.expect(TokenKind::KwIn);
        let first = self.parse_expr();
        let iter = if self.kind() == TokenKind::Range {
            self.advance();
            let end = self.parse_expr();
            ForIter::Range(first, end)
        } else {
            // `for v in xs:` iterates array elements.
            ForIter::Each(first)
        };
        if self.kind() == TokenKind::Colon {
            self.advance();
        }
        let body = self.parse_block();
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::For { var, iter, body },
            span,
        })
    }

    fn parse_return(&mut self, start: usize) -> Option<Stmt> {
        self.advance();
        let value = if self.kind() == TokenKind::Newline
            || self.kind() == TokenKind::Dedent
            || self.kind() == TokenKind::Eof
        {
            None
        } else {
            Some(self.parse_expr())
        };
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::Return { value },
            span,
        })
    }

    fn parse_import(&mut self, start: usize) -> Option<Stmt> {
        self.advance(); // consume `import`
        if self.depth > 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "P015",
                    "`import` is only allowed at the top level of a module",
                )
                .with_span(self.peek().span),
            );
        }
        let path = if self.kind() == TokenKind::Str {
            let t = self.peek().text.clone();
            self.advance();
            t
        } else {
            self.expect(TokenKind::Str);
            String::new()
        };
        let span = Span::new(self.peek().span.file, start, self.prev_span_end());
        Some(Stmt {
            kind: StmtKind::Import { path },
            span,
        })
    }

    fn parse_block(&mut self) -> Vec<Stmt> {
        while self.kind() == TokenKind::Newline {
            self.advance();
        }
        self.depth += 1;
        if self.kind() != TokenKind::Indent {
            let out = if let Some(s) = self.parse_stmt() {
                vec![s]
            } else {
                Vec::new()
            };
            self.depth -= 1;
            return out;
        }
        self.advance();
        let mut stmts = Vec::new();
        while self.kind() != TokenKind::Dedent && self.kind() != TokenKind::Eof {
            self.skip_newlines();
            if self.kind() == TokenKind::Dedent || self.kind() == TokenKind::Eof {
                break;
            }
            if let Some(s) = self.parse_stmt() {
                stmts.push(s);
            } else {
                self.advance();
            }
        }
        if self.kind() == TokenKind::Dedent {
            self.advance();
        }
        self.depth -= 1;
        stmts
    }

    // ---- expressions ----
    fn mk(&self, kind: ExprKind, start: usize) -> Expr {
        Expr {
            kind,
            span: Span::new(self.peek().span.file, start, self.prev_span_end()),
        }
    }

    fn parse_expr(&mut self) -> Expr {
        self.parse_or()
    }
    fn parse_or(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_and();
        while self.kind() == TokenKind::KwOr {
            self.advance();
            let r = self.parse_and();
            e = self.mk(
                ExprKind::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_and(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_eq();
        while self.kind() == TokenKind::KwAnd {
            self.advance();
            let r = self.parse_eq();
            e = self.mk(
                ExprKind::Binary {
                    op: BinOp::And,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_eq(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_cmp();
        loop {
            let op = match self.kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let r = self.parse_cmp();
            e = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_cmp(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_add();
        loop {
            let op = match self.kind() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::Le,
                TokenKind::GtEq => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let r = self.parse_add();
            e = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_add(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_mul();
        loop {
            let op = match self.kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let r = self.parse_mul();
            e = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_mul(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_unary();
        loop {
            let op = match self.kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let r = self.parse_unary();
            e = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(e),
                    rhs: Box::new(r),
                },
                start,
            );
        }
        e
    }
    fn parse_unary(&mut self) -> Expr {
        let start = self.peek().span.start;
        match self.kind() {
            TokenKind::Minus => {
                self.advance();
                let e = self.parse_unary();
                self.mk(
                    ExprKind::Unary {
                        op: UnOp::Neg,
                        expr: Box::new(e),
                    },
                    start,
                )
            }
            TokenKind::KwNot => {
                self.advance();
                let e = self.parse_unary();
                self.mk(
                    ExprKind::Unary {
                        op: UnOp::Not,
                        expr: Box::new(e),
                    },
                    start,
                )
            }
            _ => self.parse_postfix(),
        }
    }
    /// Postfix chains: field access `expr.field` and indexing `expr[index]`.
    fn parse_postfix(&mut self) -> Expr {
        let start = self.peek().span.start;
        let mut e = self.parse_primary();
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.advance();
                    if self.kind() != TokenKind::Ident {
                        self.diagnostics.push(
                            Diagnostic::error("P006", "expected field name after `.`")
                                .with_span(self.peek().span),
                        );
                        break;
                    }
                    let name = self.peek().text.clone();
                    self.advance();
                    e = self.mk(
                        ExprKind::Field {
                            base: Box::new(e),
                            name,
                        },
                        start,
                    );
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr();
                    if self.kind() != TokenKind::RBracket {
                        self.diagnostics.push(
                            Diagnostic::error("P010", "expected `]` after index")
                                .with_span(self.peek().span),
                        );
                        break;
                    }
                    self.advance();
                    e = self.mk(
                        ExprKind::Index {
                            base: Box::new(e),
                            index: Box::new(index),
                        },
                        start,
                    );
                }
                _ => break,
            }
        }
        e
    }
    fn parse_primary(&mut self) -> Expr {
        let start = self.peek().span.start;
        match self.kind() {
            TokenKind::Int => {
                let t = self.peek().text.clone();
                self.advance();
                self.mk(ExprKind::Int(t.parse().unwrap_or(0)), start)
            }
            TokenKind::Float => {
                let t = self.peek().text.clone();
                self.advance();
                self.mk(ExprKind::Float(t.parse().unwrap_or(0.0)), start)
            }
            TokenKind::KwTrue => {
                self.advance();
                self.mk(ExprKind::Bool(true), start)
            }
            TokenKind::KwFalse => {
                self.advance();
                self.mk(ExprKind::Bool(false), start)
            }
            TokenKind::Str => {
                let t = self.peek().text.clone();
                self.advance();
                self.mk(ExprKind::Str(t), start)
            }
            TokenKind::Ident => {
                let name = self.peek().text.clone();
                self.advance();
                if self.kind() == TokenKind::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    while self.kind() != TokenKind::RParen && self.kind() != TokenKind::Eof {
                        args.push(self.parse_expr());
                        if self.kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen);
                    self.mk(ExprKind::Call { callee: name, args }, start)
                } else {
                    self.mk(ExprKind::Var(name), start)
                }
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr();
                self.expect(TokenKind::RParen);
                e
            }
            TokenKind::LBracket => {
                // Array literal: `[e0, e1, ...]` (possibly empty).
                self.advance();
                let mut elems = Vec::new();
                while self.kind() != TokenKind::RBracket && self.kind() != TokenKind::Eof {
                    elems.push(self.parse_expr());
                    if self.kind() == TokenKind::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                if self.kind() != TokenKind::RBracket {
                    self.diagnostics.push(
                        Diagnostic::error("P014", "expected `]` to close array literal")
                            .with_span(self.peek().span),
                    );
                } else {
                    self.advance();
                }
                self.mk(ExprKind::Array(elems), start)
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("P004", format!("unexpected token {:?}", self.kind()))
                        .with_span(self.peek().span),
                );
                self.advance();
                self.mk(ExprKind::Int(0), start)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_lexer::lex;
    use nexum_source::SourceMap;
    use std::path::PathBuf;

    fn parse_str(text: &str) -> ParseResult {
        let mut sm = SourceMap::new();
        let fid = sm.add(PathBuf::from("test.nx"), text.to_string());
        let out = lex(sm.get(fid));
        assert!(
            out.diagnostics.is_empty(),
            "lex errors: {:?}",
            out.diagnostics
        );
        parse(&out.tokens)
    }

    fn first_fn_body(pr: &ParseResult) -> &[Stmt] {
        match &pr.module.stmts[0].kind {
            StmtKind::Fn { body, .. } => body,
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn mul_binds_tighter_than_add() {
        let pr = parse_str("fn main():\n    let x = 1 + 2 * 3\n");
        assert!(pr.diagnostics.is_empty(), "{:?}", pr.diagnostics);
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::Let { value, .. } => match &value.kind {
                ExprKind::Binary {
                    op: BinOp::Add,
                    rhs,
                    ..
                } => match &rhs.kind {
                    ExprKind::Binary { op: BinOp::Mul, .. } => {}
                    other => panic!("expected Mul on rhs, got {:?}", other),
                },
                other => panic!("expected Add at top, got {:?}", other),
            },
            other => panic!("expected let, got {:?}", other),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        let pr = parse_str("fn main():\n    let x = (1 + 2) * 3\n");
        assert!(pr.diagnostics.is_empty(), "{:?}", pr.diagnostics);
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::Let { value, .. } => match &value.kind {
                ExprKind::Binary {
                    op: BinOp::Mul,
                    lhs,
                    ..
                } => match &lhs.kind {
                    ExprKind::Binary { op: BinOp::Add, .. } => {}
                    other => panic!("expected Add on lhs, got {:?}", other),
                },
                other => panic!("expected Mul at top, got {:?}", other),
            },
            other => panic!("expected let, got {:?}", other),
        }
    }

    #[test]
    fn assignment_is_a_distinct_statement() {
        let pr = parse_str("fn main():\n    x = 1\n");
        assert!(pr.diagnostics.is_empty(), "{:?}", pr.diagnostics);
        let body = first_fn_body(&pr);
        assert!(matches!(body[0].kind, StmtKind::Assign { .. }));
    }

    #[test]
    fn let_mut_is_recorded() {
        let pr = parse_str("fn main():\n    let mut x = 1\n");
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::Let { mutable, .. } => assert!(*mutable),
            other => panic!("expected let, got {:?}", other),
        }
        let pr = parse_str("fn main():\n    let x = 1\n");
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::Let { mutable, .. } => assert!(!*mutable),
            other => panic!("expected let, got {:?}", other),
        }
    }

    #[test]
    fn every_node_carries_a_span() {
        let pr = parse_str("fn main():\n    let x = 1\n    print(x)\n");
        let fs = &pr.module.stmts[0];
        assert!(fs.span.end > fs.span.start);
        let body = first_fn_body(&pr);
        for s in body {
            assert!(s.span.end >= s.span.start);
        }
        match &body[0].kind {
            StmtKind::Let { value, .. } => assert!(value.span.end > value.span.start),
            other => panic!("expected let, got {:?}", other),
        }
        match &body[1].kind {
            StmtKind::Expr(e) => assert!(e.span.end > e.span.start),
            other => panic!("expected expr statement, got {:?}", other),
        }
    }

    #[test]
    fn for_range_parses() {
        let pr = parse_str("fn main():\n    for i in 0..5:\n        print(i)\n");
        assert!(pr.diagnostics.is_empty(), "{:?}", pr.diagnostics);
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::For { var, .. } => assert_eq!(var, "i"),
            other => panic!("expected for, got {:?}", other),
        }
    }

    #[test]
    fn if_else_parses() {
        let pr =
            parse_str("fn main():\n    if true:\n        print(1)\n    else:\n        print(2)\n");
        assert!(pr.diagnostics.is_empty(), "{:?}", pr.diagnostics);
        let body = first_fn_body(&pr);
        match &body[0].kind {
            StmtKind::If { else_branch, .. } => assert!(else_branch.is_some()),
            other => panic!("expected if, got {:?}", other),
        }
    }
}
