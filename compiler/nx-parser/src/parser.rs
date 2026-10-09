//! Parser core: token stream, lookahead, and the program/block entry points.

use crate::ParseError;
use nx_ast::{Program, Stmt};
use nx_lexer::{Token, TokenKind};

pub(crate) struct Parser {
    pub(crate) tokens: Vec<Token>,
    pub(crate) pos: usize,
}

impl Parser {
    pub(crate) fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub(crate) fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    pub(crate) fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    pub(crate) fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    pub(crate) fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token, ParseError> {
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

    pub(crate) fn skip_newlines(&mut self) {
        while *self.peek_kind() == TokenKind::Newline {
            self.next();
        }
    }

    pub(crate) fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while *self.peek_kind() != TokenKind::Eof {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(Program { stmts })
    }

    pub(crate) fn parse_block(&mut self) -> Result<Vec<Stmt>, ParseError> {
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
}
