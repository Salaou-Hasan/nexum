#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Int,
    Float,
    String,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Plus,
    PlusEq,
    Minus,
    MinusEq,
    Star,
    StarEq,
    Slash,
    SlashEq,
    Equals,
    EqEq,
    Bang,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Fn,
    Return,
    Break,
    Continue,
    Import,
    From,
    As,
    True,
    False,
    And,
    Or,
    Not,
    Newline,
    Indent,
    Dedent,
    Dot,
    DotDot,
    Eof,
}

fn keyword_kind(lexeme: &str) -> Option<TokenKind> {
    match lexeme {
        "if" => Some(TokenKind::If),
        "elif" => Some(TokenKind::Elif),
        "else" => Some(TokenKind::Else),
        "while" => Some(TokenKind::While),
        "for" => Some(TokenKind::For),
        "in" => Some(TokenKind::In),
        "fn" => Some(TokenKind::Fn),
        "return" => Some(TokenKind::Return),
        "break" => Some(TokenKind::Break),
        "continue" => Some(TokenKind::Continue),
        "import" => Some(TokenKind::Import),
        "from" => Some(TokenKind::From),
        "as" => Some(TokenKind::As),
        "true" => Some(TokenKind::True),
        "false" => Some(TokenKind::False),
        "and" => Some(TokenKind::And),
        "or" => Some(TokenKind::Or),
        "not" => Some(TokenKind::Not),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lex error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for LexError {}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    fn new(source: &str) -> Self {
        // Normalize line endings so \r never appears.
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        Self {
            chars: normalized.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn lex_all(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        let mut indents: Vec<usize> = vec![0];
        let mut at_line_start = true;

        loop {
            if at_line_start {
                // Measure leading spaces. Tabs are rejected for indentation.
                let start_line = self.line;
                let mut width = 0usize;
                while self.peek() == Some(' ') {
                    self.advance();
                    width += 1;
                }
                if self.peek() == Some('\t') {
                    return Err(LexError {
                        message: "tabs not allowed for indentation, use spaces".to_string(),
                        line: self.line,
                        col: self.col,
                    });
                }
                match self.peek() {
                    // Blank line or comment-only line: emit a Newline (unless
                    // consecutive) and stay at line start. No indent change.
                    Some('\n') => {
                        self.advance();
                        // Avoid duplicate Newlines for consecutive blanks.
                        if !matches!(tokens.last().map(|t: &Token| &t.kind), Some(TokenKind::Newline)) {
                            tokens.push(Token {
                                kind: TokenKind::Newline,
                                lexeme: "\n".to_string(),
                                line: start_line,
                                col: 1,
                            });
                        }
                        continue;
                    }
                    Some('#') => {
                        while matches!(self.peek(), Some(c) if c != '\n') {
                            self.advance();
                        }
                        continue;
                    }
                    None => {
                        break;
                    }
                    _ => {
                        let top = *indents.last().unwrap();
                        if width > top {
                            indents.push(width);
                            tokens.push(Token {
                                kind: TokenKind::Indent,
                                lexeme: "<indent>".to_string(),
                                line: self.line,
                                col: 1,
                            });
                        } else if width < top {
                            while *indents.last().unwrap() > width {
                                indents.pop();
                                tokens.push(Token {
                                    kind: TokenKind::Dedent,
                                    lexeme: "<dedent>".to_string(),
                                    line: self.line,
                                    col: 1,
                                });
                            }
                            if *indents.last().unwrap() != width {
                                return Err(LexError {
                                    message: "inconsistent indentation".to_string(),
                                    line: self.line,
                                    col: 1,
                                });
                            }
                        }
                        at_line_start = false;
                        continue;
                    }
                }
            }

            // Inline trivia: spaces/tabs between tokens, # comments.
            loop {
                match self.peek() {
                    Some(' ') | Some('\t') => {
                        self.advance();
                    }
                    Some('#') => {
                        while matches!(self.peek(), Some(c) if c != '\n') {
                            self.advance();
                        }
                    }
                    _ => break,
                }
            }

            let line = self.line;
            let col = self.col;
            match self.peek() {
                None => break,
                Some('\n') => {
                    self.advance();
                    // Collapse consecutive newlines.
                    if !matches!(tokens.last().map(|t: &Token| &t.kind), Some(TokenKind::Newline)) {
                        tokens.push(Token {
                            kind: TokenKind::Newline,
                            lexeme: "\n".to_string(),
                            line,
                            col,
                        });
                    }
                    at_line_start = true;
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let lexeme = self.lex_ident();
                    let kind = keyword_kind(&lexeme).unwrap_or(TokenKind::Ident);
                    tokens.push(Token { kind, lexeme, line, col });
                }
                Some(c) if c.is_ascii_digit() => {
                    let (lexeme, kind) = self.lex_number();
                    tokens.push(Token { kind, lexeme, line, col });
                }
                Some('"') => {
                    let lexeme = self.lex_string()?;
                    tokens.push(Token { kind: TokenKind::String, lexeme, line, col });
                }
                Some('(') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::LParen, lexeme: "(".to_string(), line, col });
                }
                Some(')') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::RParen, lexeme: ")".to_string(), line, col });
                }
                Some('[') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::LBracket, lexeme: "[".to_string(), line, col });
                }
                Some(']') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::RBracket, lexeme: "]".to_string(), line, col });
                }
                Some(',') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::Comma, lexeme: ",".to_string(), line, col });
                }
                Some(':') => {
                    self.advance();
                    tokens.push(Token { kind: TokenKind::Colon, lexeme: ":".to_string(), line, col });
                }
                Some('+') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::PlusEq, lexeme: "+=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Plus, lexeme: "+".to_string(), line, col });
                    }
                }
                Some('-') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::MinusEq, lexeme: "-=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Minus, lexeme: "-".to_string(), line, col });
                    }
                }
                Some('*') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::StarEq, lexeme: "*=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Star, lexeme: "*".to_string(), line, col });
                    }
                }
                Some('/') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::SlashEq, lexeme: "/=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Slash, lexeme: "/".to_string(), line, col });
                    }
                }
                Some('!') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::NotEq, lexeme: "!=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Bang, lexeme: "!".to_string(), line, col });
                    }
                }
                Some('<') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::LtEq, lexeme: "<=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Lt, lexeme: "<".to_string(), line, col });
                    }
                }
                Some('>') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::GtEq, lexeme: ">=".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Gt, lexeme: ">".to_string(), line, col });
                    }
                }
                Some('.') => {
                    let next = self.chars.get(self.pos + 1).copied();
                    if next == Some('.') {
                        self.advance();
                        self.advance();
                        tokens.push(Token { kind: TokenKind::DotDot, lexeme: "..".to_string(), line, col });
                    } else {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::Dot, lexeme: ".".to_string(), line, col });
                    }
                }
                Some('=') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token { kind: TokenKind::EqEq, lexeme: "==".to_string(), line, col });
                    } else {
                        tokens.push(Token { kind: TokenKind::Equals, lexeme: "=".to_string(), line, col });
                    }
                }
                Some(c) => {
                    return Err(LexError {
                        message: format!("unexpected character '{c}'"),
                        line,
                        col,
                    });
                }
            }
        }

        // Unwind remaining indents at EOF.
        while indents.len() > 1 {
            indents.pop();
            tokens.push(Token {
                kind: TokenKind::Dedent,
                lexeme: "<dedent>".to_string(),
                line: self.line,
                col: self.col,
            });
        }
        tokens.push(Token {
            kind: TokenKind::Eof,
            lexeme: String::new(),
            line: self.line,
            col: self.col,
        });
        Ok(tokens)
    }

    fn lex_ident(&mut self) -> String {
        let mut s = String::new();
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
            s.push(self.advance().unwrap());
        }
        s
    }

    fn lex_number(&mut self) -> (String, TokenKind) {
        let mut s = String::new();
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            s.push(self.advance().unwrap());
        }
        if self.peek() == Some('.') {
            let after_dot = self.chars.get(self.pos + 1).copied();
            if matches!(after_dot, Some(c) if c.is_ascii_digit()) {
                s.push(self.advance().unwrap());
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    s.push(self.advance().unwrap());
                }
                return (s, TokenKind::Float);
            }
        }
        (s, TokenKind::Int)
    }

    fn lex_string(&mut self) -> Result<String, LexError> {
        let start_line = self.line;
        let start_col = self.col;
        self.advance();
        let mut s = String::from("\"");
        loop {
            match self.advance() {
                None => {
                    return Err(LexError {
                        message: "unterminated string literal".to_string(),
                        line: start_line,
                        col: start_col,
                    });
                }
                Some('"') => {
                    s.push('"');
                    break;
                }
                Some('\\') => match self.advance() {
                    None => {
                        return Err(LexError {
                            message: "unterminated string literal".to_string(),
                            line: start_line,
                            col: start_col,
                        });
                    }
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some(c) => {
                        return Err(LexError {
                            message: format!("unknown escape '\\{c}'"),
                            line: self.line,
                            col: self.col,
                        });
                    }
                },
                Some('\n') => {
                    return Err(LexError {
                        message: "unterminated string literal".to_string(),
                        line: start_line,
                        col: start_col,
                    });
                }
                Some(c) => s.push(c),
            }
        }
        Ok(s)
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(source).lex_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn hello_world() {
        assert_eq!(
            kinds("print(\"Hello, world!\")"),
            vec![
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::String,
                TokenKind::RParen,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn assignment_expr() {
        assert_eq!(
            kinds("x = 10 + 20"),
            vec![
                TokenKind::Ident,
                TokenKind::Equals,
                TokenKind::Int,
                TokenKind::Plus,
                TokenKind::Int,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn comments_and_newlines() {
        let toks = lex("# hello\nx = 1 # trailing").unwrap();
        assert!(toks.iter().any(|t| t.kind == TokenKind::Newline));
        assert!(toks.iter().any(|t| t.lexeme == "x"));
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(lex("\"abc").is_err());
    }

    #[test]
    fn float_lex() {
        assert_eq!(
            kinds("x = 10.8 + 20"),
            vec![
                TokenKind::Ident,
                TokenKind::Equals,
                TokenKind::Float,
                TokenKind::Plus,
                TokenKind::Int,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn indent_dedent() {
        assert_eq!(
            kinds("if x:\n    print(x)\nprint(0)"),
            vec![
                TokenKind::If,
                TokenKind::Ident,
                TokenKind::Colon,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Ident,
                TokenKind::RParen,
                TokenKind::Newline,
                TokenKind::Dedent,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Int,
                TokenKind::RParen,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn bad_char_errors() {
        assert!(lex("@").is_err());
    }

    #[test]
    fn tab_indent_errors() {
        assert!(lex("if x:\n\tprint(x)").is_err());
    }
}
