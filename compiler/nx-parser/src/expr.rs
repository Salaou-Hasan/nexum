//! Expression parsing: precedence climbing, postfix operators, and literals.

use crate::parser::Parser;
use crate::ParseError;
use nx_ast::{BinOp, Expr, Span, UnaryOp};
use nx_lexer::TokenKind;

fn is_min_literal(lexeme: &str) -> bool {
    if lexeme == "9223372036854775808" {
        return true;
    }
    let (digits, radix) = match lexeme.strip_prefix("0x") {
        Some(d) => (d, 16),
        None => match lexeme.strip_prefix("0o") {
            Some(d) => (d, 8),
            None => match lexeme.strip_prefix("0b") {
                Some(d) => (d, 2),
                None => return false,
            },
        },
    };
    u64::from_str_radix(digits, radix).ok() == Some(1 << 63)
}

impl Parser {
    pub(crate) fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_range()
    }

    /// `a..b`. Bound tighter than the conditional expression so
    /// `x if c else 0..n` parses as `x if c else (0..n)`, and looser than
    /// everything else so `i + 1 .. n + 1` works.
    pub(crate) fn parse_range(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_if_expr()?;
        if *self.peek_kind() == TokenKind::DotDot {
            self.next();
            self.skip_newlines();
            let end = self.parse_if_expr()?;
            let span = left.span();
            return Ok(Expr::Range {
                start: Box::new(left),
                end: Box::new(end),
                span,
            });
        }
        Ok(left)
    }

    /// `a if cond else b`, lowest precedence above assignment. Written as
    /// a separate level so it composes: `f(x if c else y)`, and
    /// `1 if a else 2 if b else 3` chains to the right.
    pub(crate) fn parse_if_expr(&mut self) -> Result<Expr, ParseError> {
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
    pub(crate) fn parse_slice_rest(&mut self, base: Expr) -> Result<Expr, ParseError> {
        self.finish_slice(base, None)
    }

    pub(crate) fn finish_slice(
        &mut self,
        base: Expr,
        from: Option<Expr>,
    ) -> Result<Expr, ParseError> {
        self.expect(TokenKind::Colon, "':' in slice")?;
        let to =
            if *self.peek_kind() == TokenKind::Colon || *self.peek_kind() == TokenKind::RBracket {
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

    pub(crate) fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while *self.peek_kind() == TokenKind::Or {
            self.next();
            let right = self.parse_and()?;
            let span = left.span();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::Or,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_not()?;
        while *self.peek_kind() == TokenKind::And {
            self.next();
            let right = self.parse_not()?;
            let span = left.span();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    /// `not` sits *below* comparison, not up at unary. That is what makes
    /// `not a in b` mean `not (a in b)` and `not a == b` mean
    /// `not (a == b)`, and it is also why `not in` can exist as a single
    /// operator. At the unary level `not a == b` would instead read as
    /// `(not a) == b`, which is almost never what was meant.
    pub(crate) fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if *self.peek_kind() == TokenKind::Not {
            let t = self.next();
            let e = self.parse_not()?;
            let span = Span {
                line: t.line,
                col: t.col,
            };
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(e),
                span,
            });
        }
        self.parse_cmp()
    }

    pub(crate) fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
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
                TokenKind::Not
                    if matches!(
                        self.tokens.get(self.pos + 1).map(|t| &t.kind),
                        Some(TokenKind::In)
                    ) =>
                {
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
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    /// `|` -- loosest of the bitwise operators, below comparisons.
    pub(crate) fn parse_bitor(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bitxor()?;
        while *self.peek_kind() == TokenKind::Pipe {
            self.next();
            let right = self.parse_bitxor()?;
            let span = left.span();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::BitOr,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_bitxor(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bitand()?;
        while *self.peek_kind() == TokenKind::Caret {
            self.next();
            let right = self.parse_bitand()?;
            let span = left.span();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::BitXor,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_bitand(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_shift()?;
        while *self.peek_kind() == TokenKind::Amp {
            self.next();
            let right = self.parse_shift()?;
            let span = left.span();
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::BitAnd,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_shift(&mut self) -> Result<Expr, ParseError> {
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
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_add(&mut self) -> Result<Expr, ParseError> {
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
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    pub(crate) fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::At => BinOp::MatMul,
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
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    /// `**` binds tighter than a prefix operator on its left, so `-2 ** 2`
    /// is `-(2 ** 2)`, but the exponent may itself be signed, so
    /// `2 ** -1` is legal. That means the base is a postfix expression and
    /// the exponent is a full unary expression -- getting this the other
    /// way round sends unary and power into a cycle.
    pub(crate) fn parse_pow(&mut self) -> Result<Expr, ParseError> {
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

    pub(crate) fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            // `not` is deliberately absent: it binds looser than
            // comparison, so it is handled in `parse_not`.
            TokenKind::Minus => {
                self.next();
                // i64::MIN has no literal spelling: `9223372036854775808`
                // overflows i64, so a unary minus in front of exactly those
                // digits folds to MIN here, in any radix spelling. Anything
                // else parses as a negated expression, as before.
                if *self.peek_kind() == TokenKind::Int {
                    let nt = self.peek().clone();
                    if is_min_literal(&nt.lexeme) {
                        self.next();
                        return Ok(Expr::Int(
                            i64::MIN,
                            Span {
                                line: nt.line,
                                col: nt.col,
                            },
                        ));
                    }
                }
                let e = self.parse_unary()?;
                let span = Span {
                    line: t.line,
                    col: t.col,
                };
                Ok(Expr::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(e),
                    span,
                })
            }
            TokenKind::Tilde => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span {
                    line: t.line,
                    col: t.col,
                };
                Ok(Expr::Unary {
                    op: UnaryOp::BitNot,
                    expr: Box::new(e),
                    span,
                })
            }
            // Unary plus is a no-op, but it is legal and round-trips
            // through code that rewrites expression trees.
            TokenKind::Plus => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span {
                    line: t.line,
                    col: t.col,
                };
                Ok(Expr::Unary {
                    op: UnaryOp::Pos,
                    expr: Box::new(e),
                    span,
                })
            }
            _ => self.parse_pow(),
        }
    }

    pub(crate) fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
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
                e = Expr::Index {
                    base: Box::new(e),
                    index: Box::new(index),
                    span,
                };
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
                e = Expr::Call {
                    callee: Box::new(e),
                    args,
                    span,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    pub(crate) fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Int => {
                self.next();
                let v = t.lexeme.parse::<i64>().map_err(|_| ParseError {
                    message: format!("invalid integer {:?}", t.lexeme),
                    line: t.line,
                    col: t.col,
                })?;
                Ok(Expr::Int(
                    v,
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::Float => {
                self.next();
                let v = t.lexeme.parse::<f64>().map_err(|_| ParseError {
                    message: format!("invalid float {:?}", t.lexeme),
                    line: t.line,
                    col: t.col,
                })?;
                Ok(Expr::Float(
                    v,
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::True => {
                self.next();
                Ok(Expr::Bool(
                    true,
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::False => {
                self.next();
                Ok(Expr::Bool(
                    false,
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::String => {
                self.next();
                Ok(Expr::Str(
                    unquote(&t.lexeme),
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::Ident => {
                self.next();
                Ok(Expr::Var(
                    t.lexeme,
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            // `self` is a keyword (so the receiver can be spelled `self`,
            // `mut self`, `own self`), but inside a body it is just an
            // ordinary binding -- the receiver. Promoting it here keeps the
            // rest of the parser free of receiver awareness.
            TokenKind::Self_ => {
                self.next();
                Ok(Expr::Var(
                    "self".to_string(),
                    Span {
                        line: t.line,
                        col: t.col,
                    },
                ))
            }
            TokenKind::None => {
                self.next();
                Ok(Expr::NoneLit(Span {
                    line: t.line,
                    col: t.col,
                }))
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
                let span = Span {
                    line: lb.line,
                    col: lb.col,
                };
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
    pub(crate) fn parse_comprehension_tail(
        &mut self,
        element: Expr,
        span: Span,
    ) -> Result<Expr, ParseError> {
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
            Expr::Range {
                start: Box::new(first),
                end: Box::new(end),
                span,
            }
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
        Ok(Expr::Comprehension {
            element: Box::new(element),
            var,
            iter: Box::new(iter),
            cond,
            span,
        })
    }

    /// `{k: v, ...}` and `{}`. Insertion order is preserved, so iterating a
    /// dict is deterministic.
    pub(crate) fn parse_dict_literal(&mut self) -> Result<Expr, ParseError> {
        let lb = self.next(); // {
        let span = Span {
            line: lb.line,
            col: lb.col,
        };
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
