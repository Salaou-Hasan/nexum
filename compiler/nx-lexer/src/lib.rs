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
    StarStar,
    StarStarEq,
    Slash,
    SlashEq,
    SlashSlash,
    SlashSlashEq,
    Percent,
    PercentEq,
    Amp,
    AmpEq,
    Pipe,
    PipeEq,
    Caret,
    CaretEq,
    Tilde,
    Shl,
    ShlEq,
    Shr,
    ShrEq,
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
    LBrace,
    RBrace,
    /// `None`, the unit value. Distinct from an empty list and from a
    /// missing key, which is what makes it useful for optionals.
    None,
    Del,
    Assert,
    /// `type` -- a field declaration. A keyword so that a bare `type`
    /// cannot be read as a call to a function named `type`, which would
    /// make `type(x: Int)` ambiguous with a parameter list.
    Type,
    /// `impl` -- a method block. Keyword for the same reason as `type`.
    Impl,
    /// `self` -- a method receiver. A full keyword: it can only ever
    /// mean the receiver, so `self = 5` is refused rather than creating
    /// a variable that shadows it confusingly.
    Self_,
    /// `mut` / `own` -- receiver and (from Stage 6) binding modifiers.
    /// Reserved now so Stage 6 needs no lexer change.
    Mut,
    Own,
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
        "None" => Some(TokenKind::None),
        "del" => Some(TokenKind::Del),
        "assert" => Some(TokenKind::Assert),
        "type" => Some(TokenKind::Type),
        "impl" => Some(TokenKind::Impl),
        "self" => Some(TokenKind::Self_),
        "mut" => Some(TokenKind::Mut),
        "own" => Some(TokenKind::Own),
        "and" => Some(TokenKind::And),
        "or" => Some(TokenKind::Or),
        "not" => Some(TokenKind::Not),
        _ => None,
    }
}

/// A line number in a source file.
///
/// This is the root of the whole position pipeline -- the lexer produces
/// the first positions, and every crate downstream carries them -- so the
/// type lives here rather than in any one consumer.
///
/// It is deliberately 32 bits while the token stream index beside it is a
/// `usize`. A position is bounded by the file it describes: 4 billion
/// lines is far more than any source file holds, and an editor exhausts
/// address space long before that. Nothing does arithmetic on a position;
/// they are read, compared and printed. Making them pointer-width instead
/// costs 8 bytes on every token *and* on every AST node, for a range no
/// input can reach.
pub type LineNo = u32;

/// A column number in a source line. Same reasoning as [`LineNo`].
pub type ColNo = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub line: LineNo,
    pub col: ColNo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub line: LineNo,
    pub col: ColNo,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lex error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for LexError {}

struct Lexer {
    chars: Vec<char>,
    /// Index into `chars`. Pointer-width on purpose: it indexes a Vec and
    /// is compared against `chars.len()`.
    pos: usize,
    line: LineNo,
    col: ColNo,
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
                }
                Some(')') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RParen,
                        lexeme: ")".to_string(),
                        line,
                        col,
                    });
                }
                Some('[') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::LBracket,
                        lexeme: "[".to_string(),
                        line,
                        col,
                    });
                }
                Some(']') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RBracket,
                        lexeme: "]".to_string(),
                        line,
                        col,
                    });
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
                }
                Some('}') => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RBrace,
                        lexeme: "}".to_string(),
                        line,
                        col,
                    });
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

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(source).lex_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source).unwrap().into_iter().map(|t| t.kind).collect()
    }
    /// The interpreted text of the first string token, without its quotes.
    fn string_of(source: &str) -> String {
        let t = lex(source)
            .unwrap()
            .into_iter()
            .find(|t| t.kind == TokenKind::String)
            .expect("a string token");
        t.lexeme[1..t.lexeme.len() - 1].to_string()
    }

    #[test]
    fn unicode_escapes_cover_every_spelling() {
        // Three spellings, one meaning. They are Python's, so a reader who
        // knows one already knows the other two.
        assert_eq!(string_of(r#""A""#), "A");
        assert_eq!(string_of(r#""\u0041""#), "A");
        assert_eq!(string_of(r#""\u{41}""#), "A");
        assert_eq!(string_of(r#""\U00000041""#), "A");
        // Hex digits are case-insensitive, as everywhere else.
        assert_eq!(string_of(r#""\u00e9""#), string_of(r#""\u00E9""#));
        // The braced and unbraced spellings of one code point agree.
        assert_eq!(string_of(r#""\u{1f389}""#), string_of(r#""\U0001F389""#));
    }

    #[test]
    fn unicode_escapes_are_bounded_and_checked() {
        // A surrogate cannot be encoded, so it is an error rather than a
        // string holding something no UTF-8 encoder will accept. Every other
        // malformed escape is an error too, rather than a quietly wrong
        // string.
        for bad in [
            r#""\uD800""#,
            r#""\uDFFF""#,
            r#""\U00110000""#,
            r#""\u12""#,
            r#""\uZZZZ""#,
            r#""\u{65e5""#,
            r#""\u{}""#,
            r#""\U0001F38""#,
        ] {
            assert!(lex(bad).is_err(), "should have been rejected: {bad}");
        }
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

    /// A comment is trivia, so it must leave no tokens and no indentation
    /// behind -- not even when it is the only thing on an over-indented
    /// line, which is a classic way to corrupt a block structure.
    #[test]
    fn comment_lines_leave_no_tokens() {
        // The Newline keeps the token stream well-formed for the parser;
        // what matters is that no Indent or Dedent escapes the comment.
        let toks = lex("# only a comment\n").unwrap();
        assert!(
            !toks
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Indent | TokenKind::Dedent)),
            "comment-only file must not shift indentation: {toks:?}"
        );
    }

    #[test]
    fn over_indented_comment_does_not_open_a_block() {
        // A 6-space comment inside a 4-space block must not register as
        // an indent, or every following line looks misaligned.
        let toks = lex("if true:\n    x = 1\n      # deep\n    y = 2\n").unwrap();
        let indents = toks.iter().filter(|t| t.kind == TokenKind::Indent).count();
        assert_eq!(indents, 1, "only the `if` body should indent: {toks:?}");
        let dedents = toks.iter().filter(|t| t.kind == TokenKind::Dedent).count();
        assert_eq!(dedents, 1, "one dedent at end of block: {toks:?}");
    }

    #[test]
    fn comment_at_eof_without_newline() {
        let toks = lex("x = 1\n# no newline after").unwrap();
        assert!(toks.iter().any(|t| t.lexeme == "x"));
    }

    #[test]
    fn hash_inside_string_is_not_a_comment() {
        // The String token keeps its quotes, so match the whole literal.
        let toks = lex("x = \"a#b\"").unwrap();
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::String && t.lexeme == "\"a#b\""),
            "string contents must survive: {toks:?}"
        );
    }

    #[test]
    fn comment_may_contain_quotes_and_hashes() {
        let toks = lex("x = 1 # it's a \"test\" ###").unwrap();
        assert!(toks.iter().any(|t| t.lexeme == "x"));
        assert_eq!(
            toks.iter().filter(|t| t.kind == TokenKind::Ident).count(),
            1
        );
    }

    /// Every operator must lex as exactly one token. A two-character form
    /// splitting in two would turn `a ** b` into `a * (*b)` and fail much
    /// later, in the parser, with a confusing message.
    #[test]
    fn operators_lex_as_single_tokens() {
        let cases: &[(&str, TokenKind)] = &[
            ("**", TokenKind::StarStar),
            ("**=", TokenKind::StarStarEq),
            ("//", TokenKind::SlashSlash),
            ("//=", TokenKind::SlashSlashEq),
            ("%", TokenKind::Percent),
            ("%=", TokenKind::PercentEq),
            ("&", TokenKind::Amp),
            ("&=", TokenKind::AmpEq),
            ("|", TokenKind::Pipe),
            ("|=", TokenKind::PipeEq),
            ("^", TokenKind::Caret),
            ("^=", TokenKind::CaretEq),
            ("~", TokenKind::Tilde),
            ("<<", TokenKind::Shl),
            ("<<=", TokenKind::ShlEq),
            (">>", TokenKind::Shr),
            (">>=", TokenKind::ShrEq),
            ("{", TokenKind::LBrace),
            ("}", TokenKind::RBrace),
        ];
        for (src, want) in cases {
            let toks = lex(src).unwrap_or_else(|e| panic!("{src} failed to lex: {e:?}"));
            let first = toks[0].kind.clone();
            assert_eq!(first, *want, "{src} lexed as {first:?}, expected {want:?}");
            assert_eq!(toks.len(), 2, "{src} should be one token plus Eof");
        }
    }

    /// `<` is both a comparison and half a shift, so the two-character
    /// forms have to be tried in the right order.
    #[test]
    fn shift_and_compare_do_not_collide() {
        assert_eq!(kinds("a < b")[1], TokenKind::Lt);
        assert_eq!(kinds("a <= b")[1], TokenKind::LtEq);
        assert_eq!(kinds("a << b")[1], TokenKind::Shl);
        assert_eq!(kinds("a <<= b")[1], TokenKind::ShlEq);
        assert_eq!(kinds("a > b")[1], TokenKind::Gt);
        assert_eq!(kinds("a >= b")[1], TokenKind::GtEq);
        assert_eq!(kinds("a >> b")[1], TokenKind::Shr);
        assert_eq!(kinds("a >>= b")[1], TokenKind::ShrEq);
    }

    /// `**` must win over `*`, or exponentiation becomes multiplication
    /// followed by a dereference that does not exist.
    #[test]
    fn star_star_beats_star() {
        assert_eq!(kinds("a ** b")[1], TokenKind::StarStar);
        assert_eq!(kinds("a * b")[1], TokenKind::Star);
    }

    #[test]
    fn underscore_separators_are_stripped() {
        let toks = lex("x = 1_000_000").unwrap();
        let lit = toks.iter().find(|t| t.kind == TokenKind::Int).unwrap();
        assert_eq!(lit.lexeme, "1000000");
    }

    #[test]
    fn radix_prefixes_decode_to_decimal() {
        for (src, want) in [
            ("0xff", "255"),
            ("0o17", "15"),
            ("0b1011", "11"),
            ("0xFF", "255"),
            ("0b1010_1010", "170"),
        ] {
            let toks = lex(&format!("x = {src}")).unwrap();
            let lit = toks.iter().find(|t| t.kind == TokenKind::Int).unwrap();
            assert_eq!(lit.lexeme, want, "{src} decoded wrong");
        }
    }

    #[test]
    fn radix_overflow_keeps_its_prefix() {
        // The digits alone would parse as a *decimal* number below and
        // silently become a different value (`0x8000000000000000` read as
        // 8000000000000000), so the prefix goes back on: a prefixed
        // lexeme can never parse as i64, and the parser reports the
        // range error instead. The one exception is exactly 2^63 under a
        // unary minus, which the parser folds to i64::MIN.
        for (src, want) in [
            ("0x8000000000000000", "0x8000000000000000"),
            ("0o1000000000000000000000", "0o1000000000000000000000"),
            (
                "0b1000000000000000000000000000000000000000000000000000000000000000",
                "0b1000000000000000000000000000000000000000000000000000000000000000",
            ),
        ] {
            let toks = lex(&format!("x = {src}")).unwrap();
            let lit = toks.iter().find(|t| t.kind == TokenKind::Int).unwrap();
            assert_eq!(lit.lexeme, want, "{src} lost its prefix");
        }
    }

    #[test]
    fn exponent_literals_are_floats() {
        for src in ["1e10", "2.5e3", "1E-4", "7e+2"] {
            let toks = lex(&format!("x = {src}")).unwrap();
            assert!(
                toks.iter().any(|t| t.kind == TokenKind::Float),
                "{src} should be a float"
            );
        }
    }

    /// `1..n` is a range, so a `.` only starts a fraction when a digit
    /// follows it.
    #[test]
    fn range_is_not_a_float() {
        let toks = lex("for i in 1..5:").unwrap();
        assert!(toks.iter().any(|t| t.kind == TokenKind::DotDot));
        assert!(
            !toks.iter().any(|t| t.kind == TokenKind::Float),
            "1..5 must not lex as a float"
        );
    }

    #[test]
    fn none_is_a_keyword() {
        // `x = None` lexes as Ident, Equals, None.
        assert_eq!(kinds("x = None")[2], TokenKind::None);
        // Still a normal identifier, so a prefix does not shadow it.
        assert_eq!(kinds("x = NoneOf")[2], TokenKind::Ident);
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

    /// `impl`, `self`, `mut` and `own` are keywords, so a receiver can be
    /// spelled. They must not lex as identifiers: `self` in particular is
    /// promoted to a variable by the parser, and a program that declares
    /// `self = 1` should be a type error rather than a shadow.
    #[test]
    fn receiver_keywords_lex_as_keywords() {
        assert_eq!(
            kinds("impl T:\n    fn m(mut self):\n        return self"),
            vec![
                TokenKind::Impl,
                TokenKind::Ident,
                TokenKind::Colon,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Fn,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Mut,
                TokenKind::Self_,
                TokenKind::RParen,
                TokenKind::Colon,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Return,
                TokenKind::Self_,
                TokenKind::Dedent,
                TokenKind::Dedent,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn own_self_is_a_receiver_spelling() {
        assert_eq!(
            kinds("fn m(own self)"),
            vec![
                TokenKind::Fn,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Own,
                TokenKind::Self_,
                TokenKind::RParen,
                TokenKind::Eof
            ]
        );
    }

    /// `selfish` and `mutable` are ordinary names: keyword matching is
    /// whole-word, not a prefix test.
    #[test]
    fn keyword_prefixes_stay_identifiers() {
        assert_eq!(
            kinds("selfish = 1\nmutate = 2"),
            vec![
                TokenKind::Ident,
                TokenKind::Equals,
                TokenKind::Int,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Equals,
                TokenKind::Int,
                TokenKind::Eof
            ]
        );
    }
}
