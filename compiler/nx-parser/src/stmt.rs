//! Statement parsing: blocks, declarations, control flow, and assignment.

use crate::parser::Parser;
use crate::ParseError;
use nx_ast::{BinOp, Expr, ForIter, Span, Stmt, Target};
use nx_lexer::{Token, TokenKind};

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

impl Parser {
    pub(crate) fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
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
                Ok(Stmt::Break {
                    span: Span {
                        line: t.line,
                        col: t.col,
                    },
                })
            }
            TokenKind::Continue => {
                let t = self.next();
                Ok(Stmt::Continue {
                    span: Span {
                        line: t.line,
                        col: t.col,
                    },
                })
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
    pub(crate) fn parse_del(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // del
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        let mut targets = vec![self.parse_target()?];
        while *self.peek_kind() == TokenKind::Comma {
            self.next();
            targets.push(self.parse_target()?);
        }
        Ok(Stmt::Del { targets, span })
    }

    pub(crate) fn parse_assert(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // assert
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        let cond = self.parse_expr()?;
        let message = if *self.peek_kind() == TokenKind::Comma {
            self.next();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Stmt::Assert {
            cond,
            message,
            span,
        })
    }

    /// `impl Point:` then an indented block of `fn` definitions. Each
    /// method's first parameter may be a receiver (`self`, `mut self`,
    /// `own self`); anything else must be a plain name. A `self` anywhere
    /// but first is refused -- it can only ever mean the receiver.
    pub(crate) fn parse_impl(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // impl
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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
        Ok(Stmt::Impl {
            type_name: name,
            methods,
            span,
        })
    }

    pub(crate) fn parse_method(&mut self) -> Result<nx_ast::Method, ParseError> {
        let kw = self.next(); // fn
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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
        Ok(nx_ast::Method {
            name,
            receiver,
            params,
            body,
            span,
        })
    }

    /// A write position: a name, `a[i]`, or `p.x`. Postfix suffixes are
    /// consumed greedily, so this is really "an expression, then keep the
    /// index/attr chain", which is what makes `a[i][j] = v` work.
    pub(crate) fn parse_target(&mut self) -> Result<Target, ParseError> {
        let e = self.parse_postfix()?;
        target_from_expr(e)
    }

    /// `type Point:` then an indented block of `x: Float` lines. Indent
    /// based like every other block in the language, so a declaration
    /// reads the same way a function body does.
    ///
    /// The body is parsed line by line rather than through `parse_block`
    /// because a field line is not an expression: it is `name: Type`.
    pub(crate) fn parse_type_decl(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // type
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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

    pub(crate) fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // if
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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
        Ok(Stmt::If {
            cond,
            then_body,
            elifs,
            else_body,
            span,
        })
    }

    pub(crate) fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // while
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        let cond = self.parse_expr()?;
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body, span })
    }

    pub(crate) fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // for
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        let var_tok = self.expect(TokenKind::Ident, "loop variable")?;
        self.expect(TokenKind::In, "'in'")?;
        let first = self.parse_expr()?;
        // A range in the header stays a `ForIter::Range`, which the
        // backend emits as a counted loop rather than materialising a
        // list. Anywhere else it is just an ordinary list-valued
        // expression.
        let iter = match first {
            Expr::Range { start, end, .. } => ForIter::Range {
                start: *start,
                end: *end,
            },
            other => ForIter::Each(other),
        };
        self.expect(TokenKind::Colon, "':'")?;
        let body = self.parse_block()?;
        Ok(Stmt::For {
            var: var_tok.lexeme,
            iter,
            body,
            span,
        })
    }

    pub(crate) fn parse_fn(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // fn
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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
        Ok(Stmt::Fn {
            name: name_tok.lexeme,
            params,
            body,
            span,
        })
    }

    pub(crate) fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // return
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        match self.peek_kind() {
            TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof => Ok(Stmt::Return {
                values: Vec::new(),
                span,
            }),
            _ => {
                // `return a, b` is a tuple return; `return a` is not.
                let values = self.parse_expr_list()?;
                Ok(Stmt::Return { values, span })
            }
        }
    }

    /// A comma-separated expression list, used by `return a, b`, tuple
    /// literals and multiple assignment on the right-hand side.
    pub(crate) fn parse_expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.parse_expr()?];
        while *self.peek_kind() == TokenKind::Comma {
            self.next();
            self.skip_newlines();
            out.push(self.parse_expr()?);
        }
        Ok(out)
    }

    pub(crate) fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // import
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
        let module = self.expect(TokenKind::Ident, "module name")?.lexeme;
        let alias = if *self.peek_kind() == TokenKind::As {
            self.next();
            Some(self.expect(TokenKind::Ident, "alias")?.lexeme)
        } else {
            None
        };
        Ok(Stmt::Import {
            module,
            alias,
            span,
        })
    }

    pub(crate) fn parse_from_import(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.next(); // from
        let span = Span {
            line: kw.line,
            col: kw.col,
        };
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
        Ok(Stmt::FromImport {
            module,
            names,
            span,
        })
    }

    pub(crate) fn parse_simple_stmt(&mut self) -> Result<Stmt, ParseError> {
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
    pub(crate) fn try_parse_assignment(&mut self) -> Result<Option<Stmt>, ParseError> {
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
                TokenKind::AtEq => Some(BinOp::MatMul),
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
                    return Ok(Some(Stmt::AssignOp {
                        target: target_from_expr(first)?,
                        op,
                        value,
                        span,
                    }));
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
    pub(crate) fn try_named_assignment(
        &mut self,
        first: Token,
        start: usize,
    ) -> Result<Option<Stmt>, ParseError> {
        let span = Span {
            line: first.line,
            col: first.col,
        };
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
            Some(TokenKind::AtEq) => Some(BinOp::MatMul),
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
            return self
                .finish_multiple_assign(names.into_iter().map(Target::Name).collect(), span);
        }
        self.pos = start;
        Ok(None)
    }

    /// Parse the right-hand side of an assignment and build the statement.
    /// One value assigns to one target; several either pair up positionally
    /// or destructure a single tuple.
    pub(crate) fn finish_multiple_assign(
        &mut self,
        targets: Vec<Target>,
        span: Span,
    ) -> Result<Option<Stmt>, ParseError> {
        let values = if targets.len() == 1 && !matches!(self.peek_kind(), TokenKind::Comma) {
            vec![self.parse_expr()?]
        } else {
            self.parse_expr_list()?
        };
        Ok(Some(Stmt::Assign {
            targets,
            values,
            span,
        }))
    }
}
