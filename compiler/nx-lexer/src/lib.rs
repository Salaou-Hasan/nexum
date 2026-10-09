//! Lexer: Nexum source text to tokens.
//!
//! The lexer turns source text into a token stream with indentation-based
//! block structure (`Indent` / `Dedent`), string escapes and numeric
//! literals. Token types live in [`tokens`]; the scanner lives in [`lexer`].

mod lexer;
mod tokens;

#[cfg(test)]
mod tests;

pub use tokens::{ColNo, LexError, LineNo, Token, TokenKind};

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    lexer::Lexer::new(source).lex_all()
}
