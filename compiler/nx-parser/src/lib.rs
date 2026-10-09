//! Parser: tokens to AST.

use crate::parser::Parser;
use nx_ast::Program;
use nx_lexer::Token;

mod expr;
mod parser;
mod stmt;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parse error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for ParseError {}

pub fn parse(tokens: Vec<Token>) -> Result<Program, ParseError> {
    Parser::new(tokens).parse_program()
}

pub fn parse_source(source: &str) -> Result<Program, String> {
    let tokens = nx_lexer::lex(source).map_err(|e| e.to_string())?;
    parse(tokens).map_err(|e| e.to_string())
}
