//! Scanner: source text to tokens with indentation-based blocks.
//!
//! `Lexer` walks the characters once, emitting `Newline` / `Indent` /
//! `Dedent` for block structure plus identifiers, numbers, strings and
//! operators. The entry point `lex` in the crate root drives it.

use crate::tokens::{keyword_kind, ColNo, LexError, LineNo, Token, TokenKind};

pub(crate) struct Lexer {
    chars: Vec<char>,
    /// Index into `chars`. Pointer-width on purpose: it indexes a Vec and
    /// is compared against `chars.len()`.
    pos: usize,
    line: LineNo,
    col: ColNo,
}

impl Lexer {
    pub(crate) fn new(source: &str) -> Self {
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

    pub(crate) fn lex_all(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        let mut indents: Vec<usize> = vec![0];
        let mut at_line_start = true;
        // Bracket depth: the number of unclosed `(`, `[` and `{` seen so far.
        // Inside brackets a newline does not terminate an expression and a
        // change of column does not open or close a block, so no Indent or
        // Dedent token may be emitted until depth drops back to 0. Without
        // this, every bracket left open at a line break emitted an Indent
        // into the token stream and the first indented continuation line
        // was a parse error: `expected expression, found Indent "<indent>"`.
        // Strings and comments are consumed whole elsewhere, so a bracket
        // inside either never reaches this counter.
        let mut depth = 0usize;

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
                        if !matches!(
                            tokens.last().map(|t: &Token| &t.kind),
                            Some(TokenKind::Newline)
                        ) {
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
                        // Inside brackets the column is free: continuation
                        // lines neither open a block nor dedent their parent,
                        // and the indent stack is left untouched so the block
                        // structure resumes exactly after the closing bracket.
                        if depth == 0 {
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
                    if !matches!(
                        tokens.last().map(|t: &Token| &t.kind),
                        Some(TokenKind::Newline)
                    ) {
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
                    tokens.push(Token {
                        kind,
                        lexeme,
                        line,
                        col,
                    });
                }
                Some(c) if c.is_ascii_digit() => {
                    let (lexeme, kind) = self.lex_number();
                    tokens.push(Token {
                        kind,
                        lexeme,
                        line,
                        col,
                    });
                }
                Some('"') => {
                    let lexeme = self.lex_string()?;
                    tokens.push(Token {
                        kind: TokenKind::String,
                        lexeme,
                        line,
                        col,
                    });
                }
                Some('(') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::LParen,
                        lexeme: "(".to_string(),
                        line,
                        col,
                    });
                    depth += 1;
                }
                Some(')') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RParen,
                        lexeme: ")".to_string(),
                        line,
                        col,
                    });
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                Some('[') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::LBracket,
                        lexeme: "[".to_string(),
                        line,
                        col,
                    });
                    depth += 1;
                }
                Some(']') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RBracket,
                        lexeme: "]".to_string(),
                        line,
                        col,
                    });
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                Some(',') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::Comma,
                        lexeme: ",".to_string(),
                        line,
                        col,
                    });
                }
                Some(':') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::Colon,
                        lexeme: ":".to_string(),
                        line,
                        col,
                    });
                }
                Some('+') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::PlusEq,
                            lexeme: "+=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Plus,
                            lexeme: "+".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('-') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::MinusEq,
                            lexeme: "-=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Minus,
                            lexeme: "-".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('*') => {
                    self.advance();
                    match self.peek() {
                        Some('=') => {
                            self.advance();
                            tokens.push(Token {
                                kind: TokenKind::StarEq,
                                lexeme: "*=".to_string(),
                                line,
                                col,
                            });
                        }
                        Some('*') => {
                            self.advance();
                            if self.peek() == Some('=') {
                                self.advance();
                                tokens.push(Token {
                                    kind: TokenKind::StarStarEq,
                                    lexeme: "**=".to_string(),
                                    line,
                                    col,
                                });
                            } else {
                                tokens.push(Token {
                                    kind: TokenKind::StarStar,
                                    lexeme: "**".to_string(),
                                    line,
                                    col,
                                });
                            }
                        }
                        _ => tokens.push(Token {
                            kind: TokenKind::Star,
                            lexeme: "*".to_string(),
                            line,
                            col,
                        }),
                    }
                }
                Some('/') => {
                    self.advance();
                    match self.peek() {
                        Some('=') => {
                            self.advance();
                            tokens.push(Token {
                                kind: TokenKind::SlashEq,
                                lexeme: "/=".to_string(),
                                line,
                                col,
                            });
                        }
                        // `//` is floor division. A comment is `#`, so there
                        // is no ambiguity with a line comment here.
                        Some('/') => {
                            self.advance();
                            if self.peek() == Some('=') {
                                self.advance();
                                tokens.push(Token {
                                    kind: TokenKind::SlashSlashEq,
                                    lexeme: "//=".to_string(),
                                    line,
                                    col,
                                });
                            } else {
                                tokens.push(Token {
                                    kind: TokenKind::SlashSlash,
                                    lexeme: "//".to_string(),
                                    line,
                                    col,
                                });
                            }
                        }
                        _ => tokens.push(Token {
                            kind: TokenKind::Slash,
                            lexeme: "/".to_string(),
                            line,
                            col,
                        }),
                    }
                }
                Some('%') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::PercentEq,
                            lexeme: "%=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Percent,
                            lexeme: "%".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('&') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::AmpEq,
                            lexeme: "&=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Amp,
                            lexeme: "&".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('|') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::PipeEq,
                            lexeme: "|=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Pipe,
                            lexeme: "|".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('^') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::CaretEq,
                            lexeme: "^=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Caret,
                            lexeme: "^".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('~') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::Tilde,
                        lexeme: "~".to_string(),
                        line,
                        col,
                    });
                }
                Some('!') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::NotEq,
                            lexeme: "!=".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Bang,
                            lexeme: "!".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('<') => {
                    self.advance();
                    // `<<` and `<<=` must be tried before `<=`, and `<`
                    // stays a comparison, so there is no `<<=`/`<<`/`<=`
                    // overlap: the second character decides.
                    match self.peek() {
                        Some('=') => {
                            self.advance();
                            tokens.push(Token {
                                kind: TokenKind::LtEq,
                                lexeme: "<=".to_string(),
                                line,
                                col,
                            });
                        }
                        Some('<') => {
                            self.advance();
                            if self.peek() == Some('=') {
                                self.advance();
                                tokens.push(Token {
                                    kind: TokenKind::ShlEq,
                                    lexeme: "<<=".to_string(),
                                    line,
                                    col,
                                });
                            } else {
                                tokens.push(Token {
                                    kind: TokenKind::Shl,
                                    lexeme: "<<".to_string(),
                                    line,
                                    col,
                                });
                            }
                        }
                        _ => tokens.push(Token {
                            kind: TokenKind::Lt,
                            lexeme: "<".to_string(),
                            line,
                            col,
                        }),
                    }
                }
                Some('>') => {
                    self.advance();
                    match self.peek() {
                        Some('=') => {
                            self.advance();
                            tokens.push(Token {
                                kind: TokenKind::GtEq,
                                lexeme: ">=".to_string(),
                                line,
                                col,
                            });
                        }
                        Some('>') => {
                            self.advance();
                            if self.peek() == Some('=') {
                                self.advance();
                                tokens.push(Token {
                                    kind: TokenKind::ShrEq,
                                    lexeme: ">>=".to_string(),
                                    line,
                                    col,
                                });
                            } else {
                                tokens.push(Token {
                                    kind: TokenKind::Shr,
                                    lexeme: ">>".to_string(),
                                    line,
                                    col,
                                });
                            }
                        }
                        _ => tokens.push(Token {
                            kind: TokenKind::Gt,
                            lexeme: ">".to_string(),
                            line,
                            col,
                        }),
                    }
                }
                Some('{') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::LBrace,
                        lexeme: "{".to_string(),
                        line,
                        col,
                    });
                    depth += 1;
                }
                Some('}') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RBrace,
                        lexeme: "}".to_string(),
                        line,
                        col,
                    });
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                Some('.') => {
                    let next = self.chars.get(self.pos + 1).copied();
                    if next == Some('.') {
                        self.advance();
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::DotDot,
                            lexeme: "..".to_string(),
                            line,
                            col,
                        });
                    } else {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Dot,
                            lexeme: ".".to_string(),
                            line,
                            col,
                        });
                    }
                }
                Some('=') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::EqEq,
                            lexeme: "==".to_string(),
                            line,
                            col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Equals,
                            lexeme: "=".to_string(),
                            line,
                            col,
                        });
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

    /// Lex a numeric literal.
    ///
    /// Underscores are allowed as digit separators (`1_000_000`) and are
    /// stripped here, so every consumer sees a plain digit string. Beyond
    /// decimal there are `0x` hex, `0o` octal and `0b` binary, and floats
    /// accept an exponent (`1e10`, `2.5e-3`).
    fn lex_number(&mut self) -> (String, TokenKind) {
        // Radix prefixes. The digits after 0x/0o/0b are not decimal, so
        // they are collected separately and marked by the kind.
        if self.peek() == Some('0') {
            let marker = self.chars.get(self.pos + 1).copied();
            let radix = match marker {
                Some('x') | Some('X') => Some(16u32),
                Some('o') | Some('O') => Some(8u32),
                Some('b') | Some('B') => Some(2u32),
                _ => None,
            };
            if let Some(radix) = radix {
                self.advance();
                self.advance();
                let mut digits = String::new();
                while matches!(self.peek(), Some(c) if c.is_digit(radix) || c == '_') {
                    let c = self.advance().unwrap();
                    if c != '_' {
                        digits.push(c);
                    }
                }
                if digits.is_empty() {
                    return ("0".to_string(), TokenKind::Int);
                }
                // Decode to decimal here so the parser only ever sees a
                // plain integer. On overflow the prefix goes back on:
                // bare digits would parse as a *decimal* number below and
                // silently become a different value (`0x8000000000000000`
                // read as 8000000000000000), while a prefixed lexeme can
                // never parse as i64, so the parser reports the range
                // error instead. The one exception is exactly 2^63 under
                // a unary minus, which the parser folds to i64::MIN.
                return match i64::from_str_radix(&digits, radix) {
                    Ok(v) => (v.to_string(), TokenKind::Int),
                    Err(_) => {
                        let prefix = match radix {
                            16 => "0x",
                            8 => "0o",
                            _ => "0b",
                        };
                        (format!("{prefix}{digits}"), TokenKind::Int)
                    }
                };
            }
        }

        let mut s = String::new();
        let mut is_float = false;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
            let c = self.advance().unwrap();
            if c != '_' {
                s.push(c);
            }
        }
        // A `.` starts a fraction only when a digit follows, so `1..n`
        // still lexes as a range.
        if self.peek() == Some('.') {
            let after_dot = self.chars.get(self.pos + 1).copied();
            if matches!(after_dot, Some(c) if c.is_ascii_digit()) {
                is_float = true;
                s.push(self.advance().unwrap());
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
                    let c = self.advance().unwrap();
                    if c != '_' {
                        s.push(c);
                    }
                }
            }
        }
        // Exponent, with an optional sign. `1e10` is a float; a lone `e`
        // is not part of a number, so this only fires when digits follow.
        if matches!(self.peek(), Some('e') | Some('E')) {
            let digit_ok = match self.chars.get(self.pos + 1).copied() {
                Some(c) if c.is_ascii_digit() => true,
                Some('+') | Some('-') => {
                    matches!(self.chars.get(self.pos + 2).copied(), Some(d) if d.is_ascii_digit())
                }
                _ => false,
            };
            if digit_ok {
                is_float = true;
                s.push(self.advance().unwrap());
                if matches!(self.peek(), Some('+') | Some('-')) {
                    s.push(self.advance().unwrap());
                }
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
                    let c = self.advance().unwrap();
                    if c != '_' {
                        s.push(c);
                    }
                }
            }
        }
        (
            s,
            if is_float {
                TokenKind::Float
            } else {
                TokenKind::Int
            },
        )
    }

    /// Read the digits of a unicode escape: four hex digits for \u, eight
    /// for \U, or a brace-delimited run for anything either can hold.
    /// Python's, so anyone who knows one already knows the other.
    ///
    /// This exists so source files can stay ASCII. A literal glyph in a test
    /// or an example is written in UTF-8, and every tool that touches the file
    /// -- a diff, a terminal with the wrong code page, a patch applied as
    /// bytes -- then has to agree on the encoding, and disagreeing shows up as
    /// mojibake rather than as an error. A literal glyph cannot do that;
    /// six-character escape can, because every byte of it says what it is.
    fn read_hex_escape(&mut self, line: u32, col: u32, digits: usize) -> Result<u32, LexError> {
        let braced = self.peek() == Some('{');
        if braced {
            self.advance();
        }
        let mut v: u32 = 0;
        let mut n = 0usize;
        loop {
            match self.peek() {
                Some('}') if braced => {
                    self.advance();
                    if n == 0 {
                        return Err(LexError {
                            message: "\\u{} is empty".to_string(),
                            line,
                            col,
                        });
                    }
                    return Ok(v);
                }
                Some(c) if c.is_ascii_hexdigit() => {
                    self.advance();
                    v = v * 16 + c.to_digit(16).expect("checked above");
                    n += 1;
                    if !braced && n == digits {
                        return Ok(v);
                    }
                }
                _ => {
                    return Err(LexError {
                        message: if braced {
                            "\\u{...} needs hex digits and a closing brace".to_string()
                        } else {
                            format!("unicode escape needs exactly {digits} hex digits")
                        },
                        line,
                        col,
                    });
                }
            }
        }
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
                    // Python spells both cases: \uXXXX and \UXXXXXXXX.
                    Some('u') => {
                        let cp = self.read_hex_escape(start_line, start_col, 4)?;
                        match char::from_u32(cp) {
                            Some(c) => s.push(c),
                            None => {
                                return Err(LexError {
                                    message: format!("\\u{cp:x} is not a Unicode scalar value"),
                                    line: start_line,
                                    col: start_col,
                                });
                            }
                        }
                    }
                    Some('U') => {
                        let cp = self.read_hex_escape(start_line, start_col, 8)?;
                        match char::from_u32(cp) {
                            Some(c) => s.push(c),
                            None => {
                                return Err(LexError {
                                    message: format!("\\U{cp:x} is not a Unicode scalar value"),
                                    line: start_line,
                                    col: start_col,
                                });
                            }
                        }
                    }
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
