use nx_ast::{BinOp, Expr, ForIter, Program, Span, Stmt, Target, UnaryOp};
use nx_lexer::{Token, TokenKind};

/// Reinterpret a parsed expression as an assignment target.
///
/// docs/grammar.md 2.5: "A target is a name, an element, or a field."
/// Only those three shapes can be written, and anything else is rejected here
/// so the diagnostic names the offending source instead of surfacing much
/// later as a confusing "cannot assign".
///
/// The previous version's comment said exactly that and then did the opposite:
/// its catch-all produced `Target::Name(String::new())`, so `xs[1:3] = 9`
/// assigned to a variable with an empty name. No diagnostic, no effect, and a
/// program that read as though it worked. A slice is a value rather than a
/// place, so there is nothing to store into.
fn target_from_expr(e: Expr) -> Result<Target, ParseError> {
    let span = e.span();
    match e {
        Expr::Var(name, _) => Ok(Target::Name(name)),
        Expr::Index { base, index, .. } => Ok(Target::Index { base, index }),
        Expr::Attr { base, attr, .. } => Ok(Target::Attr { base, field: attr }),
        Expr::Slice { .. } => Err(ParseError {
            message: "cannot assign to a slice; a target is a name, an element, or a field"
                .to_string(),
            line: span.line,
            col: span.col,
        }),
        other => Err(ParseError {
            message: format!(
                "cannot assign to {}; a target is a name, an element, or a field",
                describe_expr(&other)
            ),
            line: span.line,
            col: span.col,
        }),
    }
}

/// A short human name for an expression, for diagnostics only.
fn describe_expr(e: &Expr) -> &'static str {
    match e {
        Expr::Int(..) => "an integer",
        Expr::Float(..) => "a float",
        Expr::Bool(..) => "a bool",
        Expr::Str(..) => "a string",
        Expr::NoneLit(..) => "none",
        Expr::List(..) => "a list",
        Expr::Dict(..) => "a dict",
        Expr::Range { .. } => "a range",
        Expr::Call { .. } => "a call",
        Expr::Comprehension { .. } => "a comprehension",
        Expr::IfExpr { .. } => "a conditional",
        Expr::Unary { .. } => "a unary expression",
        Expr::Binary { .. } => "a binary expression",
        _ => "this expression",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token, ParseError> {
        let t = self.peek().clone();
        if t.kind == kind {
            Ok(self.next())
        } else {
            Err(ParseError {
                message: format!("expected {what}, found {:?} {:?}", t.kind, t.lexeme),
                line: t.line,
                col: t.col,
            })
        }
    }

    fn skip_newlines(&mut self) {
        while *self.peek_kind() == TokenKind::Newline {
            self.next();
        }
    }

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while *self.peek_kind() != TokenKind::Eof {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(Program { stmts })
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, ParseError> {
        // Python-style: ':' Newline Indent stmts Dedent
        self.skip_newlines();
        self.expect(TokenKind::Indent, "indented block")?;
        self.skip_newlines();
        let mut stmts = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::Dedent | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::Dedent, "end of block")?;
        if stmts.is_empty() {
            let t = self.peek().clone();
            return Err(ParseError {
                message: "empty block".to_string(),
                line: t.line,
                col: t.col,
            });
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek_kind() {
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Fn => self.parse_fn(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Import => self.parse_import(),
            TokenKind::From => self.parse_from_import(),
            TokenKind::Break => {
                let t = self.next();
                Ok(Stmt::Break { span: Span { line: t.line, col: t.col } })
            }
            TokenKind::Continue => {
                let t = self.next();
                Ok(Stmt::Continue { span: Span { line: t.line, col: t.col } })
            }
            TokenKind::Del => self.parse_del(),
            TokenKind::Assert => self.parse_assert(),
            TokenKind::Type => self.parse_type_decl(),
            TokenKind::Impl => self.parse_impl(),
            _ => self.parse_simple_stmt(),
        }
    }

    /// `del a`, `del a[i]`, `del p.x`. `del` on a plain name unbinds it;
    /// on an element or field it removes that one entry.
    fn parse_del(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // del
        let span = Span { line: kw.line, col: kw.col };
        let mut targets = vec![self.parse_target()?];
        while *self.peek_kind() == TokenKind::Comma {
            self.next();
            targets.push(self.parse_target()?);
        }
        Ok(Stmt::Del { targets, span })
    }

    fn parse_assert(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // assert
        let span = Span { line: kw.line, col: kw.col };
        let cond = self.parse_expr()?;
        let message = if *self.peek_kind() == TokenKind::Comma {
            self.next();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Stmt::Assert { cond, message, span })
    }

    /// `impl Point:` then an indented block of `fn` definitions. Each
    /// method's first parameter may be a receiver (`self`, `mut self`,
    /// `own self`); anything else must be a plain name. A `self` anywhere
    /// but first is refused -- it can only ever mean the receiver.
    fn parse_impl(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // impl
        let span = Span { line: kw.line, col: kw.col };
        let name = self.expect(TokenKind::Ident, "type name")?.lexeme;
        self.expect(TokenKind::Colon, "':' after type name")?;
        self.expect(TokenKind::Newline, "newline before impl body")?;
        self.expect(TokenKind::Indent, "indented impl body")?;
        let mut methods = Vec::new();
        loop {
            if *self.peek_kind() == TokenKind::Dedent || *self.peek_kind() == TokenKind::Eof {
                break;
            }
            // Only `fn` definitions live in an impl block. Anything else
            // (an assignment, a stray expression) is a clear error rather
            // than a silently ignored line.
            if *self.peek_kind() != TokenKind::Fn {
                let t = self.peek().clone();
                return Err(ParseError {
                    message: format!(
                        "only `fn` definitions allowed in impl block, found {:?} {:?}",
                        t.kind, t.lexeme
                    ),
                    line: t.line,
                    col: t.col,
                });
            }
            methods.push(self.parse_method()?);
        }
        if *self.peek_kind() == TokenKind::Dedent {
            self.next();
        }
        if methods.is_empty() {
            return Err(ParseError {
                message: format!("impl '{name}' has no methods"),
                line: kw.line,
                col: kw.col,
            });
        }
        let mut seen = std::collections::HashSet::new();
        for m in &methods {
            if !seen.insert(m.name.clone()) {
                return Err(ParseError {
                    message: format!("duplicate method '{}' in impl '{name}'", m.name),
                    line: m.span.line,
                    col: m.span.col,
                });
            }
        }
        Ok(Stmt::Impl { type_name: name, methods, span })
    }

    fn parse_method(&mut self) -> Result<nx_ast::Method, ParseError> {
        let kw = self.next(); // fn
        let span = Span { line: kw.line, col: kw.col };
        let name = self.expect(TokenKind::Ident, "method name")?.lexeme;
        self.expect(TokenKind::LParen, "'('")?;
        let mut receiver = nx_ast::ReceiverKind::None;
        let mut params = Vec::new();
        self.skip_newlines();
        if *self.peek_kind() != TokenKind::RParen {
            // First parameter position: a receiver or a plain name.
            match self.peek_kind() {
                TokenKind::Self_ => {
                    self.next();
                    receiver = nx_ast::ReceiverKind::Read;
                }
                TokenKind::Mut => {
                    self.next();
                    self.expect(TokenKind::Self_, "'self' after 'mut'")?;
                    receiver = nx_ast::ReceiverKind::Mut;
                }
                TokenKind::Own => {
                    self.next();
                    self.expect(TokenKind::Self_, "'self' after 'own'")?;
                    receiver = nx_ast::ReceiverKind::Own;
                }
                _ => {
                    let p = self.expect(TokenKind::Ident, "parameter")?;
                    params.push(p.lexeme);
                }
            }
            // A receiver consumes the first position: what follows must be
            // `,` + more params or `)`.
            self.skip_newlines();
            if *self.peek_kind() == TokenKind::Comma {
                self.next();
                self.skip_newlines();
            }
            loop {
                match self.peek_kind() {
                    TokenKind::RParen => break,
                    TokenKind::Self_ | TokenKind::Mut | TokenKind::Own => {
                        let t = self.peek().clone();
                        return Err(ParseError {
                            message: "'self' is only allowed as the first parameter".to_string(),
                            line: t.line,
                            col: t.col,
                        });
                    }
                    _ => {
                        let p = self.expect(TokenKind::Ident, "parameter")?;
                        params.push(p.lexeme);
                        self.skip_newlines();
                        if *self.peek_kind() == TokenKind::Comma {
                            self.next();
                            self.skip_newlines();
                            continue;
                        }
                        break;
                    }
                }
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(nx_ast::Method { name, receiver, params, body, span })
    }

    /// A write position: a name, `a[i]`, or `p.x`. Postfix suffixes are
    /// consumed greedily, so this is really "an expression, then keep the
    /// index/attr chain", which is what makes `a[i][j] = v` work.
    fn parse_target(&mut self) -> Result<Target, ParseError> {
        let e = self.parse_postfix()?;
        target_from_expr(e)
    }

    /// `type Point:` then an indented block of `x: Float` lines. Indent
    /// based like every other block in the language, so a declaration
    /// reads the same way a function body does.
    ///
    /// The body is parsed line by line rather than through `parse_block`
    /// because a field line is not an expression: it is `name: Type`.
    fn parse_type_decl(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // type
        let span = Span { line: kw.line, col: kw.col };
        let name = self.expect(TokenKind::Ident, "type name")?.lexeme;
        self.expect(TokenKind::Colon, "':' after type name")?;
        self.expect(TokenKind::Newline, "newline before type body")?;
        self.expect(TokenKind::Indent, "indented type body")?;
        let mut fields = Vec::new();
        loop {
            if *self.peek_kind() == TokenKind::Dedent || *self.peek_kind() == TokenKind::Eof {
                break;
            }
            let fname = self.expect(TokenKind::Ident, "field name")?.lexeme;
            // The type is optional: `x` alone declares an unresolved
            // field, which is how a field's type gets pinned later by how
            // it is used rather than being written out in advance.
            let ty = if *self.peek_kind() == TokenKind::Colon {
                self.next();
                self.expect(TokenKind::Ident, "field type")?.lexeme
            } else {
                "Any".to_string()
            };
            if fields.iter().any(|f: &nx_ast::Field| f.name == fname) {
                return Err(ParseError {
                    message: format!("duplicate field '{fname}' in type '{name}'"),
                    line: kw.line,
                    col: kw.col,
                });
            }
            fields.push(nx_ast::Field { name: fname, ty });
            // A field line ends at the newline; there is no comma form,
            // so a stray one is a clear error rather than being ignored.
            if *self.peek_kind() == TokenKind::Newline {
                self.next();
            }
        }
        if *self.peek_kind() == TokenKind::Dedent {
            self.next();
        }
        if fields.is_empty() {
            return Err(ParseError {
                message: format!("type '{name}' has no fields"),
                line: kw.line,
                col: kw.col,
            });
        }
        Ok(Stmt::TypeDecl { name, fields, span })
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // if
        let span = Span { line: kw.line, col: kw.col };
        let cond = self.parse_expr()?;
        self.expect(TokenKind::Colon, "':'")?;
        let then_body = self.parse_block()?;
        self.skip_newlines();
        let mut elifs = Vec::new();
        while *self.peek_kind() == TokenKind::Elif {
            self.next();
            let c = self.parse_expr()?;
            self.expect(TokenKind::Colon, "':'")?;
            let b = self.parse_block()?;
            elifs.push((c, b));
            self.skip_newlines();
        }
        let else_body = if *self.peek_kind() == TokenKind::Else {
            self.next();
            self.expect(TokenKind::Colon, "':'")?;
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt::If { cond, then_body, elifs, else_body, span })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // while
        let span = Span { line: kw.line, col: kw.col };
        let cond = self.parse_expr()?;
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body, span })
    }


    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // for
        let span = Span { line: kw.line, col: kw.col };
        let var_tok = self.expect(TokenKind::Ident, "loop variable")?;
        self.expect(TokenKind::In, "'in'")?;
        let first = self.parse_expr()?;
        // A range in the header stays a `ForIter::Range`, which the
        // backend emits as a counted loop rather than materialising a
        // list. Anywhere else it is just an ordinary list-valued
        // expression.
        let iter = match first {
            Expr::Range { start, end, .. } => ForIter::Range { start: *start, end: *end },
            other => ForIter::Each(other),
        };
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(Stmt::For { var: var_tok.lexeme, iter, body, span })
    }

    fn parse_fn(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // fn
        let span = Span { line: kw.line, col: kw.col };
        let name_tok = self.expect(TokenKind::Ident, "function name")?;
        self.expect(TokenKind::LParen, "'('")?;
        let mut params = Vec::new();
        self.skip_newlines();
        if *self.peek_kind() != TokenKind::RParen {
            loop {
                let p = self.expect(TokenKind::Ident, "parameter")?;
                params.push(p.lexeme);
                self.skip_newlines();
                if *self.peek_kind() == TokenKind::Comma {
                    self.next();
                    self.skip_newlines();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(Stmt::Fn { name: name_tok.lexeme, params, body, span })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // return
        let span = Span { line: kw.line, col: kw.col };
        match self.peek_kind() {
            TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof => {
                Ok(Stmt::Return { values: Vec::new(), span })
            }
            _ => {
                // `return a, b` is a tuple return; `return a` is not.
                let values = self.parse_expr_list()?;
                Ok(Stmt::Return { values, span })
            }
        }
    }

    /// A comma-separated expression list, used by `return a, b`, tuple
    /// literals and multiple assignment on the right-hand side.
    fn parse_expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.parse_expr()?];
        while *self.peek_kind() == TokenKind::Comma {
            self.next();
            self.skip_newlines();
            out.push(self.parse_expr()?);
        }
        Ok(out)
    }

    fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // import
        let span = Span { line: kw.line, col: kw.col };
        let module = self.expect(TokenKind::Ident, "module name")?.lexeme;
        let alias = if *self.peek_kind() == TokenKind::As {
            self.next();
            Some(self.expect(TokenKind::Ident, "alias")?.lexeme)
        } else {
            None
        };
        Ok(Stmt::Import { module, alias, span })
    }

    fn parse_from_import(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // from
        let span = Span { line: kw.line, col: kw.col };
        let module = self.expect(TokenKind::Ident, "module name")?.lexeme;
        self.expect(TokenKind::Import, "'import'")?;
        let mut names = Vec::new();
        loop {
            let name = self.expect(TokenKind::Ident, "name")?.lexeme;
            let alias = if *self.peek_kind() == TokenKind::As {
                self.next();
                Some(self.expect(TokenKind::Ident, "alias")?.lexeme)
            } else {
                None
            };
            names.push((name, alias));
            if *self.peek_kind() == TokenKind::Comma {
                self.next();
                continue;
            }
            break;
        }
        Ok(Stmt::FromImport { module, names, span })
    }

    fn parse_simple_stmt(&mut self) -> Result<Stmt, ParseError> {
        // print(...) -> Stmt::Print
        if self.peek().kind == TokenKind::Ident && self.peek().lexeme == "print" {
            let save = self.pos;
            self.next(); // print
            if *self.peek_kind() == TokenKind::LParen {
                let span = Span {
                    line: self.tokens[save].line,
                    col: self.tokens[save].col,
                };
                self.next(); // (
                self.skip_newlines();
                let mut values = Vec::new();
                if *self.peek_kind() != TokenKind::RParen {
                    loop {
                        values.push(self.parse_expr()?);
                        self.skip_newlines();
                        if *self.peek_kind() == TokenKind::Comma {
                            self.next();
                            self.skip_newlines();
                            continue;
                        }
                        break;
                    }
                }
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'")?;
                return Ok(Stmt::Print { values, span });
            }
            self.pos = save;
        }

        // Assignment: `t = v`, `a[i] = v`, `p.x = v`, `a, b = f()`, and the
        // compound forms of all of those.
        if let Some(stmt) = self.try_parse_assignment()? {
            return Ok(stmt);
        }

        Ok(Stmt::Expr(self.parse_expr()?))
    }

    /// Attempt an assignment at the current position, rewinding if this
    /// turns out to be a plain expression instead. Every target form ends
    /// in `=` or `op=`, so the decision can be made without backtracking
    /// except for the rare `a[i] = v` shape.
    fn try_parse_assignment(&mut self) -> Result<Option<Stmt>, ParseError> {
        let start = self.pos;
        let first = self.peek().clone();
        // `self` opens an assignment target for the same reason an
        // identifier does: `self.x = v` and `self[i] = v` are the mutations
        // a `mut self` method exists to make. It is a keyword, so it needs
        // the same target treatment spelled out here.
        let name_like = matches!(first.kind, TokenKind::Ident | TokenKind::Self_);

        // An identifier is either an assignment head or the start of an
        // expression; decide by looking at what follows the target chain.
        if first.kind == TokenKind::Ident {
            if let Some(stmt) = self.try_named_assignment(first.clone(), start)? {
                return Ok(Some(stmt));
            }
        }
        // `a[i] = v` / `p.x = v` / `a[i] += v` cannot start with a bare
        // identifier that is itself the whole left side, so parse a
        // postfix expression and see whether a `=` follows it.
        if name_like {
            let first = self.parse_postfix()?;
            // Several element or field targets against several values:
            // `xs[0], xs[1] = 7, 8`. Bare-name lists never reach here --
            // `try_named_assignment` decides those by lookahead above --
            // but a mixed list (`a, xs[0] = 1, 2`) does.
            if *self.peek_kind() == TokenKind::Comma {
                let span = first.span();
                let mut exprs = vec![first];
                while *self.peek_kind() == TokenKind::Comma {
                    self.next(); // ,
                    self.skip_newlines();
                    exprs.push(self.parse_postfix()?);
                }
                if *self.peek_kind() == TokenKind::Equals {
                    self.next(); // =
                    let mut targets = Vec::with_capacity(exprs.len());
                    for e in exprs {
                        targets.push(target_from_expr(e)?);
                    }
                    return self.finish_multiple_assign(targets, span);
                }
                // Not an assignment after all: rewind so the statement
                // parses (and fails, if it must) as an expression.
                self.pos = start;
                return Ok(None);
            }
            let compound = match self.peek_kind() {
                TokenKind::PlusEq => Some(BinOp::Add),
                TokenKind::MinusEq => Some(BinOp::Sub),
                TokenKind::StarEq => Some(BinOp::Mul),
                TokenKind::SlashEq => Some(BinOp::Div),
                TokenKind::SlashSlashEq => Some(BinOp::FloorDiv),
                TokenKind::PercentEq => Some(BinOp::Mod),
                TokenKind::StarStarEq => Some(BinOp::Pow),
                TokenKind::AmpEq => Some(BinOp::BitAnd),
                TokenKind::PipeEq => Some(BinOp::BitOr),
                TokenKind::CaretEq => Some(BinOp::BitXor),
                TokenKind::ShlEq => Some(BinOp::Shl),
                TokenKind::ShrEq => Some(BinOp::Shr),
                _ => None,
            };
            if compound.is_some() || *self.peek_kind() == TokenKind::Equals {
                let span = first.span();
                if let Some(op) = compound {
                    self.next(); // op=
                    let value = self.parse_expr()?;
                    return Ok(Some(Stmt::AssignOp { target: target_from_expr(first)?, op, value, span }));
                }
                self.next(); // =
                return self.finish_multiple_assign(vec![target_from_expr(first)?], span);
            }
        }
        self.pos = start;
        Ok(None)
    }

    /// `name = ...` and `name op= ...`, including `a, b = ...` where the
    /// first name is the head of a multiple assignment.
    fn try_named_assignment(
        &mut self,
        first: Token,
        start: usize,
    ) -> Result<Option<Stmt>, ParseError> {
        let span = Span { line: first.line, col: first.col };
        // Look past a `name,` prefix to find the real `=`.
        let mut probe = self.pos;
        let mut names = vec![first.lexeme.clone()];
        while self.tokens.get(probe + 1).map(|t| &t.kind) == Some(&TokenKind::Comma) {
            match self.tokens.get(probe + 2).map(|t| &t.kind) {
                Some(TokenKind::Ident) => {
                    names.push(self.tokens[probe + 2].lexeme.clone());
                    probe += 2;
                }
                _ => break,
            }
        }
        let after = self.tokens.get(probe + 1).map(|t| &t.kind);
        let compound = match after {
            Some(TokenKind::PlusEq) => Some(BinOp::Add),
            Some(TokenKind::MinusEq) => Some(BinOp::Sub),
            Some(TokenKind::StarEq) => Some(BinOp::Mul),
            Some(TokenKind::SlashEq) => Some(BinOp::Div),
            Some(TokenKind::SlashSlashEq) => Some(BinOp::FloorDiv),
            Some(TokenKind::PercentEq) => Some(BinOp::Mod),
            Some(TokenKind::StarStarEq) => Some(BinOp::Pow),
            Some(TokenKind::AmpEq) => Some(BinOp::BitAnd),
            Some(TokenKind::PipeEq) => Some(BinOp::BitOr),
            Some(TokenKind::CaretEq) => Some(BinOp::BitXor),
            Some(TokenKind::ShlEq) => Some(BinOp::Shl),
            Some(TokenKind::ShrEq) => Some(BinOp::Shr),
            _ => None,
        };
        if compound.is_some() {
            self.next(); // name
            self.next(); // op=
            let value = self.parse_expr()?;
            return Ok(Some(Stmt::AssignOp {
                target: Target::Name(first.lexeme),
                op: compound.expect("checked above"),
                value,
                span,
            }));
        }
        if after == Some(&TokenKind::Equals) {
            // Consume `name` and any further `name,` pairs.
            for _ in 0..names.len() {
                self.next(); // name
                if *self.peek_kind() == TokenKind::Comma {
                    self.next();
                    self.skip_newlines();
                }
            }
            self.next(); // =
            return self.finish_multiple_assign(names.into_iter().map(Target::Name).collect(), span);
        }
        self.pos = start;
        Ok(None)
    }

    /// Parse the right-hand side of an assignment and build the statement.
    /// One value assigns to one target; several either pair up positionally
    /// or destructure a single tuple.
    fn finish_multiple_assign(
        &mut self,
        targets: Vec<Target>,
        span: Span,
    ) -> Result<Option<Stmt>, ParseError> {
        let values = if targets.len() == 1 && !matches!(self.peek_kind(), TokenKind::Comma) {
            vec![self.parse_expr()?]
        } else {
            self.parse_expr_list()?
        };
        Ok(Some(Stmt::Assign { targets, values, span }))
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_range()
    }

    /// `a..b`. Bound tighter than the conditional expression so
    /// `x if c else 0..n` parses as `x if c else (0..n)`, and looser than
    /// everything else so `i + 1 .. n + 1` works.
    fn parse_range(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_if_expr()?;
        if *self.peek_kind() == TokenKind::DotDot {
            self.next();
            self.skip_newlines();
            let end = self.parse_if_expr()?;
            let span = left.span();
            return Ok(Expr::Range { start: Box::new(left), end: Box::new(end), span });
        }
        Ok(left)
    }

    /// `a if cond else b`, lowest precedence above assignment. Written as
    /// a separate level so it composes: `f(x if c else y)`, and
    /// `1 if a else 2 if b else 3` chains to the right.
    fn parse_if_expr(&mut self) -> Result<Expr, ParseError> {
        let value = self.parse_or()?;
        if *self.peek_kind() == TokenKind::If {
            self.next();
            let cond = self.parse_or()?;
            self.expect(TokenKind::Else, "'else'")?;
            let else_value = self.parse_if_expr()?;
            let span = value.span();
            return Ok(Expr::IfExpr {
                cond: Box::new(cond),
                then_value: Box::new(value),
                else_value: Box::new(else_value),
                span,
            });
        }
        Ok(value)
    }

    /// Slice after the opening bracket and an optional `from`, e.g. the
    /// `1:` in `a[1:]`.
    fn parse_slice_rest(&mut self, base: Expr) -> Result<Expr, ParseError> {
        self.finish_slice(base, None)
    }

    fn finish_slice(&mut self, base: Expr, from: Option<Expr>) -> Result<Expr, ParseError> {
        self.expect(TokenKind::Colon, "':' in slice")?;
        let to = if *self.peek_kind() == TokenKind::Colon || *self.peek_kind() == TokenKind::RBracket {
            None
        } else {
            Some(Box::new(self.parse_expr()?))
        };
        self.skip_newlines();
        let step = if *self.peek_kind() == TokenKind::Colon {
            self.next();
            self.skip_newlines();
            if *self.peek_kind() == TokenKind::RBracket {
                None
            } else {
                Some(Box::new(self.parse_expr()?))
            }
        } else {
            None
        };
        self.skip_newlines();
        self.expect(TokenKind::RBracket, "']'")?;
        let span = base.span();
        Ok(Expr::Slice {
            base: Box::new(base),
            from: from.map(Box::new),
            to,
            step,
            span,
        })
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while *self.peek_kind() == TokenKind::Or {
            self.next();
            let right = self.parse_and()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::Or, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_not()?;
        while *self.peek_kind() == TokenKind::And {
            self.next();
            let right = self.parse_not()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::And, right: Box::new(right), span };
        }
        Ok(left)
    }

    /// `not` sits *below* comparison, not up at unary. That is what makes
    /// `not a in b` mean `not (a in b)` and `not a == b` mean
    /// `not (a == b)`, and it is also why `not in` can exist as a single
    /// operator. At the unary level `not a == b` would instead read as
    /// `(not a) == b`, which is almost never what was meant.
    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if *self.peek_kind() == TokenKind::Not {
            let t = self.next();
            let e = self.parse_not()?;
            let span = Span { line: t.line, col: t.col };
            return Ok(Expr::Unary { op: UnaryOp::Not, expr: Box::new(e), span });
        }
        self.parse_cmp()
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bitor()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::GtEq => BinOp::GtEq,
                // `x in xs` / `x not in xs`. `not` is a prefix operator, so
                // the pair has to be matched together here or `not in` would
                // never be seen.
                TokenKind::In => BinOp::In,
                TokenKind::Not if matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::In)) => {
                    BinOp::NotIn
                }
                _ => break,
            };
            if op == BinOp::NotIn {
                self.next(); // not
            }
            self.next();
            let right = self.parse_bitor()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op, right: Box::new(right), span };
        }
        Ok(left)
    }

    /// `|` -- loosest of the bitwise operators, below comparisons.
    fn parse_bitor(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bitxor()?;
        while *self.peek_kind() == TokenKind::Pipe {
            self.next();
            let right = self.parse_bitxor()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::BitOr, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bitand()?;
        while *self.peek_kind() == TokenKind::Caret {
            self.next();
            let right = self.parse_bitand()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::BitXor, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_bitand(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_shift()?;
        while *self.peek_kind() == TokenKind::Amp {
            self.next();
            let right = self.parse_shift()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::BitAnd, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Shl => BinOp::Shl,
                TokenKind::Shr => BinOp::Shr,
                _ => break,
            };
            self.next();
            let right = self.parse_add()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_mul()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                // `//` has to be matched before `/`, which the lexer
                // guarantees by emitting distinct tokens.
                TokenKind::SlashSlash => BinOp::FloorDiv,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op, right: Box::new(right), span };
        }
        Ok(left)
    }

    /// `**` binds tighter than a prefix operator on its left, so `-2 ** 2`
    /// is `-(2 ** 2)`, but the exponent may itself be signed, so
    /// `2 ** -1` is legal. That means the base is a postfix expression and
    /// the exponent is a full unary expression -- getting this the other
    /// way round sends unary and power into a cycle.
    fn parse_pow(&mut self) -> Result<Expr, ParseError> {
        let base = self.parse_postfix()?;
        if *self.peek_kind() == TokenKind::StarStar {
            self.next();
            let exp = self.parse_unary()?;
            let span = base.span();
            return Ok(Expr::Binary {
                left: Box::new(base),
                op: BinOp::Pow,
                right: Box::new(exp),
                span,
            });
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            // `not` is deliberately absent: it binds looser than
            // comparison, so it is handled in `parse_not`.
            TokenKind::Minus => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span { line: t.line, col: t.col };
                Ok(Expr::Unary { op: UnaryOp::Neg, expr: Box::new(e), span })
            }
            TokenKind::Tilde => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span { line: t.line, col: t.col };
                Ok(Expr::Unary { op: UnaryOp::BitNot, expr: Box::new(e), span })
            }
            // Unary plus is a no-op, but it is legal and round-trips
            // through code that rewrites expression trees.
            TokenKind::Plus => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span { line: t.line, col: t.col };
                Ok(Expr::Unary { op: UnaryOp::Pos, expr: Box::new(e), span })
            }
            _ => self.parse_pow(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        loop {
            if *self.peek_kind() == TokenKind::LBracket {
                self.next();
                self.skip_newlines();
                // A colon in the brackets means a slice, not an index.
                if *self.peek_kind() == TokenKind::Colon {
                    e = self.parse_slice_rest(e)?;
                    continue;
                }
                let index = self.parse_expr()?;
                self.skip_newlines();
                if *self.peek_kind() == TokenKind::Colon {
                    e = self.finish_slice(e, Some(index))?;
                    continue;
                }
                self.expect(TokenKind::RBracket, "']'")?;
                let span = e.span();
                e = Expr::Index { base: Box::new(e), index: Box::new(index), span };
            } else if *self.peek_kind() == TokenKind::Dot {
                self.next();
                let attr = self.expect(TokenKind::Ident, "attribute name")?;
                let span = e.span();
                e = Expr::Attr {
                    base: Box::new(e),
                    attr: attr.lexeme,
                    span,
                };
            } else if *self.peek_kind() == TokenKind::LParen {
                self.next(); // (
                let span = e.span();
                let mut args = Vec::new();
                self.skip_newlines();
                if *self.peek_kind() != TokenKind::RParen {
                    loop {
                        args.push(self.parse_expr()?);
                        self.skip_newlines();
                        if *self.peek_kind() == TokenKind::Comma {
                            self.next();
                            self.skip_newlines();
                            continue;
                        }
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "')'")?;
                e = Expr::Call { callee: Box::new(e), args, span };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Int => {
                self.next();
                let v = t.lexeme.parse::<i64>().map_err(|_| ParseError {
                    message: format!("invalid integer {:?}", t.lexeme),
                    line: t.line,
                    col: t.col,
                })?;
                Ok(Expr::Int(v, Span { line: t.line, col: t.col }))
            }
            TokenKind::Float => {
                self.next();
                let v = t.lexeme.parse::<f64>().map_err(|_| ParseError {
                    message: format!("invalid float {:?}", t.lexeme),
                    line: t.line,
                    col: t.col,
                })?;
                Ok(Expr::Float(v, Span { line: t.line, col: t.col }))
            }
            TokenKind::True => {
                self.next();
                Ok(Expr::Bool(true, Span { line: t.line, col: t.col }))
            }
            TokenKind::False => {
                self.next();
                Ok(Expr::Bool(false, Span { line: t.line, col: t.col }))
            }
            TokenKind::String => {
                self.next();
                Ok(Expr::Str(unquote(&t.lexeme), Span { line: t.line, col: t.col }))
            }
            TokenKind::Ident => {
                self.next();
                Ok(Expr::Var(t.lexeme, Span { line: t.line, col: t.col }))
            }
            // `self` is a keyword (so the receiver can be spelled `self`,
            // `mut self`, `own self`), but inside a body it is just an
            // ordinary binding -- the receiver. Promoting it here keeps the
            // rest of the parser free of receiver awareness.
            TokenKind::Self_ => {
                self.next();
                Ok(Expr::Var(
                    "self".to_string(),
                    Span { line: t.line, col: t.col },
                ))
            }
            TokenKind::None => {
                self.next();
                Ok(Expr::NoneLit(Span { line: t.line, col: t.col }))
            }
            TokenKind::LBrace => self.parse_dict_literal(),
            TokenKind::LParen => {
                self.next();
                self.skip_newlines();
                let e = self.parse_expr()?;
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'")?;
                Ok(e)
            }
            TokenKind::LBracket => {
                let lb = self.next();
                let span = Span { line: lb.line, col: lb.col };
                self.skip_newlines();
                let mut items = Vec::new();
                if *self.peek_kind() != TokenKind::RBracket {
                    loop {
                        let first = self.parse_expr()?;
                        // `[x for y in ys]`: a `for` where a comma would go
                        // makes this a comprehension rather than a list.
                        if items.is_empty() && *self.peek_kind() == TokenKind::For {
                            return self.parse_comprehension_tail(first, span);
                        }
                        items.push(first);
                        self.skip_newlines();
                        if *self.peek_kind() == TokenKind::Comma {
                            self.next();
                            self.skip_newlines();
                            // trailing comma before ] allowed
                            if *self.peek_kind() == TokenKind::RBracket {
                                break;
                            }
                            continue;
                        }
                        break;
                    }
                }
                self.expect(TokenKind::RBracket, "']'")?;
                Ok(Expr::List(items, span))
            }
            _ => Err(ParseError {
                message: format!("expected expression, found {:?} {:?}", t.kind, t.lexeme),
                line: t.line,
                col: t.col,
            }),
        }
    }

    /// `[element for var in iter if cond]` -- the `[` and the element have
    /// been consumed; this reads the `for` tail.
    fn parse_comprehension_tail(&mut self, element: Expr, span: Span) -> Result<Expr, ParseError> {
        self.expect(TokenKind::For, "'for' in comprehension")?;
        let var = self.expect(TokenKind::Ident, "loop variable")?.lexeme;
        self.expect(TokenKind::In, "'in' in comprehension")?;
        self.skip_newlines();
        // The iterable is parsed at the `or` level, not through the conditional
        // expression: the following `if` belongs to the comprehension, not
        // to a ternary on the iterable. A range is still accepted here, so
        // `[i for i in 0..5]` works.
        let first = self.parse_or()?;
        let iter = if *self.peek_kind() == TokenKind::DotDot {
            self.next();
            self.skip_newlines();
            let end = self.parse_or()?;
            let span = first.span();
            Expr::Range { start: Box::new(first), end: Box::new(end), span }
        } else {
            first
        };
        self.skip_newlines();
        let cond = if *self.peek_kind() == TokenKind::If {
            self.next();
            self.skip_newlines();
            Some(Box::new(self.parse_or()?))
        } else {
            None
        };
        self.skip_newlines();
        self.expect(TokenKind::RBracket, "']'")?;
        Ok(Expr::Comprehension { element: Box::new(element), var, iter: Box::new(iter), cond, span })
    }

    /// `{k: v, ...}` and `{}`. Insertion order is preserved, so iterating a
    /// dict is deterministic.
    fn parse_dict_literal(&mut self) -> Result<Expr, ParseError> {
        let lb = self.next(); // {
        let span = Span { line: lb.line, col: lb.col };
        let mut pairs = Vec::new();
        self.skip_newlines();
        if *self.peek_kind() != TokenKind::RBrace {
            loop {
                self.skip_newlines();
                let key = self.parse_expr()?;
                self.skip_newlines();
                self.expect(TokenKind::Colon, "':' in dict entry")?;
                self.skip_newlines();
                let value = self.parse_expr()?;
                pairs.push((key, value));
                self.skip_newlines();
                if *self.peek_kind() == TokenKind::Comma {
                    self.next();
                    self.skip_newlines();
                    if *self.peek_kind() == TokenKind::RBrace {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RBrace, "'}'")?;
        Ok(Expr::Dict(pairs, span))
    }
}

fn unquote(lexeme: &str) -> String {
    let inner = lexeme.strip_prefix('"').unwrap_or(lexeme);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    inner.to_string()
}

pub fn parse(tokens: Vec<Token>) -> Result<Program, ParseError> {
    Parser::new(tokens).parse_program()
}

pub fn parse_source(source: &str) -> Result<Program, String> {
    let tokens = nx_lexer::lex(source).map_err(|e| e.to_string())?;
    parse(tokens).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(src: &str) -> Program {
        parse_source(src).unwrap_or_else(|e| panic!("{src:?}: {e}"))
    }

    #[test]
    fn print_string() {
        let p = prog("print(\"hi\")");
        assert_eq!(p.stmts.len(), 1);
        assert!(matches!(p.stmts[0], Stmt::Print { .. }));
    }

    #[test]
    fn assign_add() {
        let p = prog("x = 10 + 20");
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0], Target::Name("x".to_string()));
                assert!(matches!(values[0], Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn precedence_mul_binds_tighter() {
        let p = prog("x = 1 + 2 * 3");
        match &p.stmts[0] {
            Stmt::Assign { values, .. } => {
                let right = match &values[0] {
                    Expr::Binary { op: BinOp::Add, right, .. } => right,
                    other => panic!("expected +, got {other:?}"),
                };
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn generic_call() {
        let p = prog("foo(1, 2)");
        assert!(matches!(
            &p.stmts[0],
            Stmt::Expr(Expr::Call { callee, .. })
            if matches!(callee.as_ref(), Expr::Var(f, _) if f == "foo")
        ));
    }

    #[test]
    fn print_multi_arg() {
        let p = prog("print(y, x)");
        match &p.stmts[0] {
            Stmt::Print { values, .. } => assert_eq!(values.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn float_expr() {
        let p = prog("x = 10.8 + 20");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn if_else_block() {
        let p = prog("if x > 1:\n    print(x)\nelse:\n    print(0)");
        assert!(matches!(p.stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn while_block() {
        let p = prog("while x < 5:\n    x = x + 1");
        assert!(matches!(p.stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn bool_logic_precedence() {
        let p = prog("x = true and false or true");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn for_range() {
        let p = prog("for i in 0..5:\n    print(i)");
        assert!(matches!(p.stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn for_each() {
        let p = prog("for i in nums:\n    print(i)");
        assert!(matches!(p.stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn import_forms() {
        let p = prog("import utils\nimport utils as u\nfrom utils import foo, bar as b");
        assert_eq!(p.stmts.len(), 3);
        assert!(matches!(p.stmts[0], Stmt::Import { .. }));
        assert!(matches!(p.stmts[2], Stmt::FromImport { .. }));
    }

    #[test]
    fn attr_call() {
        let p = prog("print(utils.foo(1))");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn fn_def() {
        let p = prog("fn add(a, b):\n    return a + b");
        assert!(matches!(p.stmts[0], Stmt::Fn { .. }));
    }

    #[test]
    fn parallel_is_not_a_keyword() {
        // `parallel:` was removed rather than deprecated. It never had a
        // sound implementation: the scheduler proved race-freedom by static
        // approximation and the approximation had holes, so a program could
        // print the wrong answer whenever it lost a race. `parallel` is now
        // an ordinary identifier, and the old spelling is a syntax error
        // rather than a silent no-op.
        assert!(parse_source("parallel:\n    a()\n    b()\n").is_err());
        // ...and the name itself is free to use.
        assert!(parse_source("parallel = 1\nprint(parallel)\n").is_ok());
    }

    #[test]
    fn elif_chain() {
        let p = prog("if a:\n    print(1)\nelif b:\n    print(2)\nelse:\n    print(3)");
        match &p.stmts[0] {
            Stmt::If { elifs, else_body, .. } => {
                assert_eq!(elifs.len(), 1);
                assert!(else_body.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_and_index() {
        let p = prog("x = [1, 2]\nprint(x[0])");
        assert_eq!(p.stmts.len(), 2);
    }

    #[test]
    fn aug_assign() {
        let p = prog("x += 1");
        assert!(matches!(p.stmts[0], Stmt::AssignOp { .. }));
    }

    #[test]
    fn missing_paren_errors() {
        assert!(parse_source("print(\"hi\"").is_err());
    }

    // ---- Stage 1 syntax ----

    /// Every new operator has to reach the AST as itself, not as a
    /// mistokenized pair. `%` in particular was missing long enough that
    /// the benchmark suite had to work around it with `x - (x / m) * m`.
    #[test]
    fn new_operators_parse() {
        for (src, op) in [
            ("x = a % b", BinOp::Mod),
            ("x = a // b", BinOp::FloorDiv),
            ("x = a ** b", BinOp::Pow),
            ("x = a & b", BinOp::BitAnd),
            ("x = a | b", BinOp::BitOr),
            ("x = a ^ b", BinOp::BitXor),
            ("x = a << b", BinOp::Shl),
            ("x = a >> b", BinOp::Shr),
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            match &p.stmts[0] {
                Stmt::Assign { values, .. } => match &values[0] {
                    Expr::Binary { op: got, .. } => assert_eq!(*got, op, "{src}"),
                    other => panic!("{src} gave {other:?}"),
                },
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    /// `**` is right associative, so `2 ** 3 ** 2` is 2 ** 9.
    #[test]
    fn pow_is_right_associative() {
        let p = prog("x = 2 ** 3 ** 2");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Binary { op: BinOp::Pow, right, .. } => {
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Pow, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A prefix operator is looser than `**` on its left, so `-2 ** 2` is
    /// `-(2 ** 2)` and evaluates to -4. Binding it the other way would give
    /// 4 and silently change results.
    #[test]
    fn unary_is_looser_than_pow_on_its_left() {
        let p = prog("x = -2 ** 2");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::Unary { op: UnaryOp::Neg, .. }));
    }

    /// The exponent may be signed, which is the other half of why power
    /// and unary sit on opposite sides of each other.
    #[test]
    fn pow_exponent_may_be_signed() {
        let p = parse_source("x = 2 ** -1").unwrap();
        assert_eq!(p.stmts.len(), 1);
    }

    /// The bitwise ladder sits below comparison and above arithmetic, so
    /// `a < b & c` is `a < (b & c)`.
    #[test]
    fn bitwise_binds_looser_than_arithmetic() {
        let p = prog("x = a & b + c");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Binary { op: BinOp::BitAnd, right, .. } => {
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn membership_operators() {
        for (src, op) in [("x = a in b", BinOp::In), ("x = a not in b", BinOp::NotIn)] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            match &values[0] {
                Expr::Binary { op: got, .. } => assert_eq!(*got, op, "{src}"),
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    /// `not in` has to beat the prefix `not`, or `x not in y` reads as
    /// `x` followed by a dangling negation.
    #[test]
    fn membership_beats_prefix_not() {
        let p = parse_source("x = not a in b").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::Unary { op: UnaryOp::Not, .. }));
    }

    #[test]
    fn ternary_expression() {
        let p = parse_source("x = 1 if c else 2").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::IfExpr { .. }));
    }

    #[test]
    fn indexed_assignment() {
        let p = parse_source("a[0] = 5").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert!(matches!(&targets[0], Target::Index { .. }));
                assert_eq!(values.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a[i][j] = v` needs the postfix chain to be kept whole rather than
    /// stopping at the first bracket.
    #[test]
    fn nested_index_assignment() {
        let p = parse_source("a[i][j] = v").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, .. } => match &targets[0] {
                Target::Index { base, .. } => assert!(matches!(&**base, Expr::Index { .. })),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn indexed_augmented_assignment() {
        let p = parse_source("a[i] += 2").unwrap();
        match &p.stmts[0] {
            Stmt::AssignOp { target, op, .. } => {
                assert!(matches!(target, Target::Index { .. }));
                assert_eq!(*op, BinOp::Add);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A plain call must not be mistaken for an assignment just because it
    /// starts with an identifier.
    #[test]
    fn call_is_not_an_assignment() {
        let p = parse_source("f(1)").unwrap();
        assert!(matches!(&p.stmts[0], Stmt::Expr(Expr::Call { .. })));
    }

    #[test]
    fn multiple_assignment_pairs() {
        let p = parse_source("a, b = 1, 2").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a, b = f()` destructures a single tuple, so the right side stays a
    /// one-element list at parse time and the checker resolves it.
    #[test]
    fn multiple_assignment_from_call() {
        let p = parse_source("a, b = f()").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 1);
                assert!(matches!(&values[0], Expr::Call { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Element targets pair up positionally too: `xs[0], xs[1] = 7, 8`.
    /// Only bare-name lists are decided by lookahead; anything with a
    /// bracket or dot parses here, one postfix target at a time.
    #[test]
    fn multiple_element_targets_assign() {
        let p = parse_source("xs[0], xs[1] = 7, 8").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
                assert!(matches!(&targets[0], Target::Index { .. }));
                assert!(matches!(&targets[1], Target::Index { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A mixed list takes the same path: the bare-name lookahead only
    /// fires when every target is a name.
    #[test]
    fn mixed_name_and_element_targets_assign() {
        let p = parse_source("a, xs[0] = 1, 2").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, values, .. } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
                assert!(matches!(&targets[0], Target::Name(_)));
                assert!(matches!(&targets[1], Target::Index { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A trailing comma with no `=` is not an assignment: the parse
    /// rewinds and the statement fails as an expression instead.
    #[test]
    fn comma_without_equals_is_not_an_assignment() {
        assert!(parse_source("xs[0], xs[1]").is_err());
    }

    #[test]
    fn return_tuple() {
        let p = parse_source("fn f():\n    return 1, 2").unwrap();
        let stmts = match &p.stmts[0] {
            Stmt::Fn { body, .. } => body,
            other => panic!("{other:?}"),
        };
        match &stmts[0] {
            Stmt::Return { values, .. } => assert_eq!(values.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    /// Bare `return` stays a no-value return, distinct from `return None`.
    #[test]
    fn bare_return_has_no_values() {
        let p = parse_source("fn f():\n    return").unwrap();
        let stmts = match &p.stmts[0] {
            Stmt::Fn { body, .. } => body,
            other => panic!("{other:?}"),
        };
        match &stmts[0] {
            Stmt::Return { values, .. } => assert!(values.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn none_literal() {
        let p = parse_source("x = None").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::NoneLit(_)));
    }

    #[test]
    fn slices_in_every_form() {
        for src in ["x = a[1:3]", "x = a[:3]", "x = a[1:]", "x = a[:]", "x = a[::2]"] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            match &values[0] {
                Expr::Slice { .. } => {}
                other => panic!("{src} gave {other:?}"),
            }
        }
        // A step must not swallow the index.
        let p = parse_source("x = a[1:3:2]").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Slice { from, to, step, .. } => {
                assert!(from.is_some() && to.is_some() && step.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a[i]` and `a[i:j]` must not be confused: an index is not a slice
    /// with an empty range.
    #[test]
    fn index_is_not_a_slice() {
        let p = parse_source("x = a[i]").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::Index { .. }));
    }

    #[test]
    fn del_forms() {
        for src in ["del a", "del a[i]", "del p.x", "del a, b"] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            match &p.stmts[0] {
                Stmt::Del { targets, .. } => assert!(!targets.is_empty(), "{src}"),
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    #[test]
    fn assert_forms() {
        let p = parse_source("assert x > 1").unwrap();
        assert!(matches!(&p.stmts[0], Stmt::Assert { message: None, .. }));
        let p = parse_source("assert x > 1, \"too small\"").unwrap();
        match &p.stmts[0] {
            Stmt::Assert { message, .. } => assert!(message.is_some()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dict_literal() {
        let p = parse_source("d = {\"a\": 1, \"b\": 2}").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Dict(pairs, _) => assert_eq!(pairs.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_dict_literal() {
        let p = parse_source("d = {}").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Dict(pairs, _) => assert!(pairs.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn comprehensions() {
        for src in [
            "x = [i * 2 for i in ys]",
            "x = [i * 2 for i in ys if i > 1]",
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            assert!(matches!(&values[0], Expr::Comprehension { .. }), "{src}");
        }
    }

    #[test]
    fn extended_number_literals() {
        let p = parse_source("a = 1_000\nb = 0xff\nc = 1e3\nd = 2.5e-3").unwrap();
        assert_eq!(p.stmts.len(), 4);
    }

    // ---- impl blocks ----

    #[test]
    fn impl_block_parses() {
        let p = parse_source("impl Point:\n    fn area(self):\n        return 1\n    fn origin():\n        return 2").unwrap();
        match &p.stmts[0] {
            Stmt::Impl { type_name, methods, .. } => {
                assert_eq!(type_name, "Point");
                assert_eq!(methods.len(), 2);
                assert_eq!(methods[0].name, "area");
                assert_eq!(methods[0].receiver, nx_ast::ReceiverKind::Read);
                assert!(methods[0].params.is_empty());
                assert_eq!(methods[1].receiver, nx_ast::ReceiverKind::None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn impl_receivers() {
        let p = parse_source(
            "impl P:\n    fn a(self):\n        return 1\n    fn b(mut self, x):\n        return 2\n    fn c(own self):\n        return 3",
        )
        .unwrap();
        let methods = match &p.stmts[0] {
            Stmt::Impl { methods, .. } => methods,
            other => panic!("{other:?}"),
        };
        assert_eq!(methods[0].receiver, nx_ast::ReceiverKind::Read);
        assert_eq!(methods[1].receiver, nx_ast::ReceiverKind::Mut);
        assert_eq!(methods[1].params, vec!["x".to_string()]);
        assert_eq!(methods[2].receiver, nx_ast::ReceiverKind::Own);
    }

    #[test]
    fn impl_rejects_non_fn_bodies() {
        assert!(parse_source("impl P:\n    x = 1").is_err());
        assert!(parse_source("impl P:\n    print(1)").is_err());
    }

    #[test]
    fn impl_rejects_empty_and_duplicate_methods() {
        assert!(parse_source("impl P:\n").is_err());
        assert!(parse_source("impl P:\n    fn a(self):\n        return 1\n    fn a(self):\n        return 2").is_err());
    }

    #[test]
    fn self_only_first() {
        // `self` anywhere but first is refused, even though the name
        // would otherwise parse as a parameter.
        assert!(parse_source("impl P:\n    fn a(x, self):\n        return 1").is_err());
        assert!(parse_source("impl P:\n    fn a(mut self, mut self):\n        return 1").is_err());
    }

    /// `self` is a keyword, but inside a body it is just the receiver
    /// binding. Reading it, writing through it, and indexing it must all
    /// parse as ordinary variable targets, because those are exactly the
    /// operations a `mut self` method exists to perform.
    #[test]
    fn self_is_a_variable_in_a_body() {
        let p = parse_source(
            "impl P:\n    fn m(mut self):\n        self.x = 1\n        self.y = self.x + 2\n        self.items[0] = self.y\n        return self",
        )
        .unwrap();
        let body = match &p.stmts[0] {
            Stmt::Impl { methods, .. } => &methods[0].body,
            other => panic!("{other:?}"),
        };
        // First statement must be an assignment, not a bare expression:
        // that is the whole point of the promotion.
        assert!(
            matches!(body[0], Stmt::Assign { .. }),
            "self.x = 1 must parse as an assignment: {:?}",
            body[0]
        );
        // Reading `self` yields a plain variable reference.
        let ret = match &body[3] {
            Stmt::Return { values, .. } => values[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(ret.span().line, 6);
        assert!(
            matches!(&ret, Expr::Var(n, _) if n == "self"),
            "return self must be a variable read: {ret:?}"
        );
    }

    /// `q.moved(1, 1).moved(2, 2)` parses as a call on a call, which is
    /// what makes a `mut self` chain one expression.
    #[test]
    fn method_calls_chain_left() {
        let p = parse_source("q.moved(1, 1).moved(2, 2).area()").unwrap();
        match &p.stmts[0] {
            Stmt::Expr(e) => {
                let inner = match e {
                    Expr::Call { callee, .. } => callee.as_ref(),
                    other => panic!("{other:?}"),
                };
                // The receiver of the outer call is itself a call.
                assert!(
                    matches!(inner, Expr::Attr { base, .. } if matches!(base.as_ref(), Expr::Call { .. })),
                    "chained method must nest: {inner:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// `Point.origin()` is `Point` followed by a call on an attribute --
    /// the associated-function spelling, distinct from constructing
    /// `Point(...)`.
    #[test]
    fn associated_function_call_parses_as_attribute() {
        let p = parse_source("p = Point.origin()").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { values, .. } => match &values[0] {
                Expr::Call { callee, .. } => assert!(
                    matches!(callee.as_ref(), Expr::Attr { attr, .. } if attr == "origin"),
                    "callee must be an attribute: {callee:?}"
                ),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    /// An `impl` block for a name that is not a type is a checker error, not
    /// a parse error, so the parser accepts it and lets the checker report
    /// the precise reason.
    #[test]
    fn impl_of_unknown_type_parses() {
        assert!(parse_source("impl Missing:\n    fn a(self):\n        return 1").is_ok());
    }

    #[test]
    fn impl_rejects_missing_type_name() {
        assert!(parse_source("impl:\n    fn a(self):\n        return 1").is_err());
        assert!(parse_source("impl P:\n    fn (self):\n        return 1").is_err());
    }
}
