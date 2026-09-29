use nx_ast::{BinOp, Expr, ForIter, Program, Span, Stmt, UnaryOp};
use nx_lexer::{Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
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
            TokenKind::Break => {
                let t = self.next();
                Ok(Stmt::Break { span: Span { line: t.line, col: t.col } })
            }
            TokenKind::Continue => {
                let t = self.next();
                Ok(Stmt::Continue { span: Span { line: t.line, col: t.col } })
            }
            _ => self.parse_simple_stmt(),
        }
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
        let iter = if *self.peek_kind() == TokenKind::DotDot {
            self.next();
            let end = self.parse_expr()?;
            ForIter::Range { start: first, end }
        } else {
            ForIter::Each(first)
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
                Ok(Stmt::Return { value: None, span })
            }
            _ => {
                let v = self.parse_expr()?;
                Ok(Stmt::Return { value: Some(v), span })
            }
        }
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

        // x = expr, x += expr, etc.
        if self.peek().kind == TokenKind::Ident {
            let name_tok = self.peek().clone();
            let next_kind = self.tokens.get(self.pos + 1).map(|t: &Token| &t.kind);
            if next_kind == Some(&TokenKind::Equals) {
                self.next(); // name
                self.next(); // =
                let value = self.parse_expr()?;
                return Ok(Stmt::Assign {
                    name: name_tok.lexeme,
                    value,
                    span: Span { line: name_tok.line, col: name_tok.col },
                });
            }
            let op = match next_kind {
                Some(TokenKind::PlusEq) => Some(BinOp::Add),
                Some(TokenKind::MinusEq) => Some(BinOp::Sub),
                Some(TokenKind::StarEq) => Some(BinOp::Mul),
                Some(TokenKind::SlashEq) => Some(BinOp::Div),
                _ => None,
            };
            if let Some(op) = op {
                self.next(); // name
                self.next(); // op=
                let value = self.parse_expr()?;
                return Ok(Stmt::AssignOp {
                    name: name_tok.lexeme,
                    op,
                    value,
                    span: Span { line: name_tok.line, col: name_tok.col },
                });
            }
        }

        Ok(Stmt::Expr(self.parse_expr()?))
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or()
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
        let mut left = self.parse_cmp()?;
        while *self.peek_kind() == TokenKind::And {
            self.next();
            let right = self.parse_cmp()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op: BinOp::And, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::GtEq => BinOp::GtEq,
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
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            let span = left.span();
            left = Expr::Binary { left: Box::new(left), op, right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Not | TokenKind::Bang => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span { line: t.line, col: t.col };
                Ok(Expr::Unary { op: UnaryOp::Not, expr: Box::new(e), span })
            }
            TokenKind::Minus => {
                self.next();
                let e = self.parse_unary()?;
                let span = Span { line: t.line, col: t.col };
                Ok(Expr::Unary { op: UnaryOp::Neg, expr: Box::new(e), span })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        loop {
            if *self.peek_kind() == TokenKind::LBracket {
                let lb = self.next();
                self.skip_newlines();
                let index = self.parse_expr()?;
                self.skip_newlines();
                self.expect(TokenKind::RBracket, "']'")?;
                let span = e.span();
                let _ = lb;
                e = Expr::Index { base: Box::new(e), index: Box::new(index), span };
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
                let span = Span { line: t.line, col: t.col };
                if *self.peek_kind() == TokenKind::LParen {
                    self.next();
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
                    Ok(Expr::Call { func: t.lexeme, args, span })
                } else {
                    Ok(Expr::Var(t.lexeme, span))
                }
            }
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
                let mut items = Vec::new();
                self.skip_newlines();
                if *self.peek_kind() != TokenKind::RBracket {
                    loop {
                        items.push(self.parse_expr()?);
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
            Stmt::Assign { name, value, .. } => {
                assert_eq!(name, "x");
                assert!(matches!(value, Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn precedence_mul_binds_tighter() {
        let p = prog("x = 1 + 2 * 3");
        match &p.stmts[0] {
            Stmt::Assign { value: Expr::Binary { op: BinOp::Add, right, .. }, .. } => {
                assert!(matches!(**right, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn generic_call() {
        let p = prog("foo(1, 2)");
        assert!(matches!(&p.stmts[0], Stmt::Expr(Expr::Call { func, .. }) if func == "foo"));
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
    fn fn_def() {
        let p = prog("fn add(a, b):\n    return a + b");
        assert!(matches!(p.stmts[0], Stmt::Fn { .. }));
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
}
