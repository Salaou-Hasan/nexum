//! Indentation-aware lexer for Nexum (bootstrap).

use nexum_diagnostics::Diagnostic;
use nexum_source::{SourceFile, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Int,
    Float,
    Str,
    // keywords
    KwFn,
    KwType,
    KwLet,
    KwMut,
    KwIf,
    KwElse,
    KwWhile,
    KwFor,
    KwIn,
    KwReturn,
    KwTrue,
    KwFalse,
    KwAnd,
    KwOr,
    KwNot,
    KwImport,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Arrow,
    Range,
    Semicolon,
    // operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    EqEq,
    BangEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Eq,
    // layout
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl TokenKind {
    /// Stable name of the token kind, used by `nx lex` so that the
    /// self-hosted lexer can be compared against this one token by token.
    pub fn name(&self) -> &'static str {
        match self {
            TokenKind::Ident => "Ident",
            TokenKind::Int => "Int",
            TokenKind::Float => "Float",
            TokenKind::Str => "Str",
            TokenKind::KwFn => "KwFn",
            TokenKind::KwType => "KwType",
            TokenKind::KwLet => "KwLet",
            TokenKind::KwMut => "KwMut",
            TokenKind::KwIf => "KwIf",
            TokenKind::KwElse => "KwElse",
            TokenKind::KwWhile => "KwWhile",
            TokenKind::KwFor => "KwFor",
            TokenKind::KwIn => "KwIn",
            TokenKind::KwReturn => "KwReturn",
            TokenKind::KwTrue => "KwTrue",
            TokenKind::KwFalse => "KwFalse",
            TokenKind::KwAnd => "KwAnd",
            TokenKind::KwOr => "KwOr",
            TokenKind::KwNot => "KwNot",
            TokenKind::KwImport => "KwImport",
            TokenKind::LParen => "LParen",
            TokenKind::RParen => "RParen",
            TokenKind::LBracket => "LBracket",
            TokenKind::RBracket => "RBracket",
            TokenKind::Comma => "Comma",
            TokenKind::Colon => "Colon",
            TokenKind::Dot => "Dot",
            TokenKind::Arrow => "Arrow",
            TokenKind::Range => "Range",
            TokenKind::Semicolon => "Semicolon",
            TokenKind::Plus => "Plus",
            TokenKind::Minus => "Minus",
            TokenKind::Star => "Star",
            TokenKind::Slash => "Slash",
            TokenKind::Percent => "Percent",
            TokenKind::PlusEq => "PlusEq",
            TokenKind::MinusEq => "MinusEq",
            TokenKind::StarEq => "StarEq",
            TokenKind::SlashEq => "SlashEq",
            TokenKind::PercentEq => "PercentEq",
            TokenKind::EqEq => "EqEq",
            TokenKind::BangEq => "BangEq",
            TokenKind::Lt => "Lt",
            TokenKind::Gt => "Gt",
            TokenKind::LtEq => "LtEq",
            TokenKind::GtEq => "GtEq",
            TokenKind::Eq => "Eq",
            TokenKind::Newline => "Newline",
            TokenKind::Indent => "Indent",
            TokenKind::Dedent => "Dedent",
            TokenKind::Eof => "Eof",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    pub text: String,
}

pub struct LexerOutput {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(sf: &SourceFile) -> LexerOutput {
    let mut l = Lexer {
        sf,
        i: 0,
        line_start: 0,
        indent_stack: vec![0],
        at_line_start: true,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };
    l.run();
    LexerOutput {
        tokens: l.tokens,
        diagnostics: l.diagnostics,
    }
}

struct Lexer<'a> {
    sf: &'a SourceFile,
    i: usize,
    line_start: usize,
    indent_stack: Vec<usize>,
    at_line_start: bool,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn text(&self) -> &'a str {
        &self.sf.text
    }
    fn eof(&self) -> bool {
        self.i >= self.text().len()
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.sf.id, start, end)
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        let text = self.text()[start..end].to_string();
        self.tokens.push(Token {
            kind,
            span: self.span(start, end),
            text,
        });
    }

    fn run(&mut self) {
        // We'll process char by char with indentation handling.
        while !self.eof() {
            if self.at_line_start {
                // handle indentation
                // skip blank lines / comment-only lines? We'll handle: compute indent
                // Find end of line to check if blank
                let line_end = self.text()[self.i..]
                    .find('\n')
                    .map(|x| self.i + x)
                    .unwrap_or(self.text().len());
                let line_slice = &self.text()[self.i..line_end];
                let trimmed = line_slice.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    // skip this line entirely (no tokens, no indent changes)
                    self.i = line_end;
                    if self.i < self.text().len() {
                        self.i += 1;
                    } // skip \n
                      // emit newline? skip blank lines
                    continue;
                }
                // compute indent (spaces)
                let indent = line_slice.len() - trimmed.len();
                let cur = *self.indent_stack.last().unwrap();
                if indent > cur {
                    self.indent_stack.push(indent);
                    self.push(TokenKind::Indent, self.i, self.i); // zero-width marker at position
                                                                  // Actually make span point at first non-space
                }
                while indent < *self.indent_stack.last().unwrap() {
                    self.indent_stack.pop();
                    self.push(TokenKind::Dedent, self.i, self.i);
                }
                if indent != *self.indent_stack.last().unwrap() {
                    // mismatched indent
                    self.diagnostics.push(
                        Diagnostic::error("L001", "inconsistent indentation")
                            .with_span(self.span(self.i, self.i)),
                    );
                }
                // skip leading spaces
                self.i += indent;
                self.at_line_start = false;
                self.line_start = self.i;
                continue;
            }

            let c = self.text()[self.i..].chars().next().unwrap();
            let start = self.i;

            // newline
            if c == '\n' {
                self.push(TokenKind::Newline, start, start + 1);
                self.i += 1;
                self.at_line_start = true;
                continue;
            }
            if c == '\r' {
                self.i += 1;
                continue;
            }
            // whitespace
            if c.is_whitespace() {
                self.i += c.len_utf8();
                continue;
            }
            // comment
            if c == '#' {
                // skip to newline
                while !self.eof() && self.text().as_bytes()[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            // string
            if c == '"' {
                self.i += 1;
                let mut s = String::new();
                while !self.eof() {
                    let ch = self.text()[self.i..].chars().next().unwrap();
                    if ch == '"' {
                        break;
                    }
                    if ch == '\\' {
                        self.i += 1;
                        if !self.eof() {
                            let e = self.text()[self.i..].chars().next().unwrap();
                            match e {
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                '"' => s.push('"'),
                                '\\' => s.push('\\'),
                                _ => s.push(e),
                            }
                            self.i += e.len_utf8();
                        }
                    } else {
                        s.push(ch);
                        self.i += ch.len_utf8();
                    }
                }
                if self.eof() {
                    self.diagnostics.push(
                        Diagnostic::error("L002", "unterminated string")
                            .with_span(self.span(start, self.i)),
                    );
                } else {
                    self.i += 1; // closing quote
                }
                // store string content in token text (without quotes)
                self.tokens.push(Token {
                    kind: TokenKind::Str,
                    span: self.span(start, self.i),
                    text: s,
                });
                continue;
            }
            // number
            if c.is_ascii_digit() {
                let mut is_float = false;
                while !self.eof() {
                    let ch = self.text()[self.i..].chars().next().unwrap();
                    if ch.is_ascii_digit() {
                        self.i += 1;
                    } else if ch == '.' && !is_float {
                        // peek next
                        let rest = &self.text()[self.i + 1..];
                        if rest.starts_with(|x: char| x.is_ascii_digit()) {
                            is_float = true;
                            self.i += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                let kind = if is_float {
                    TokenKind::Float
                } else {
                    TokenKind::Int
                };
                self.push(kind, start, self.i);
                continue;
            }
            // ident / keyword
            if c.is_alphabetic() || c == '_' {
                while !self.eof() {
                    let ch = self.text()[self.i..].chars().next().unwrap();
                    if ch.is_alphanumeric() || ch == '_' {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let word = &self.text()[start..self.i];
                let kind = match word {
                    "fn" => TokenKind::KwFn,
                    "type" => TokenKind::KwType,
                    "let" => TokenKind::KwLet,
                    "mut" => TokenKind::KwMut,
                    "if" => TokenKind::KwIf,
                    "else" => TokenKind::KwElse,
                    "while" => TokenKind::KwWhile,
                    "for" => TokenKind::KwFor,
                    "in" => TokenKind::KwIn,
                    "return" => TokenKind::KwReturn,
                    "true" => TokenKind::KwTrue,
                    "false" => TokenKind::KwFalse,
                    "and" => TokenKind::KwAnd,
                    "or" => TokenKind::KwOr,
                    "not" => TokenKind::KwNot,
                    "import" => TokenKind::KwImport,
                    _ => TokenKind::Ident,
                };
                self.push(kind, start, self.i);
                continue;
            }
            // two-char operators
            let two = if self.i + 1 < self.text().len() {
                &self.text()[self.i..self.i + 2]
            } else {
                ""
            };
            let kind = match two {
                "==" => Some(TokenKind::EqEq),
                "!=" => Some(TokenKind::BangEq),
                "<=" => Some(TokenKind::LtEq),
                ">=" => Some(TokenKind::GtEq),
                "->" => Some(TokenKind::Arrow),
                ".." => Some(TokenKind::Range),
                "+=" => Some(TokenKind::PlusEq),
                "-=" => Some(TokenKind::MinusEq),
                "*=" => Some(TokenKind::StarEq),
                "/=" => Some(TokenKind::SlashEq),
                "%=" => Some(TokenKind::PercentEq),
                _ => None,
            };
            if let Some(k) = kind {
                self.push(k, start, self.i + 2);
                self.i += 2;
                continue;
            }
            // single char
            let k = match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ',' => TokenKind::Comma,
                ':' => TokenKind::Colon,
                '.' => TokenKind::Dot,
                ';' => TokenKind::Semicolon,
                '+' => TokenKind::Plus,
                '-' => TokenKind::Minus,
                '*' => TokenKind::Star,
                '/' => TokenKind::Slash,
                '%' => TokenKind::Percent,
                '<' => TokenKind::Lt,
                '>' => TokenKind::Gt,
                '=' => TokenKind::Eq,
                _ => {
                    self.diagnostics.push(
                        Diagnostic::error("L003", format!("unexpected character {:?}", c))
                            .with_span(self.span(start, start + c.len_utf8())),
                    );
                    self.i += c.len_utf8();
                    continue;
                }
            };
            self.push(k, start, self.i + c.len_utf8());
            self.i += c.len_utf8();
        }

        // dedent remaining
        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            let pos = self.text().len();
            self.tokens.push(Token {
                kind: TokenKind::Dedent,
                span: self.span(pos, pos),
                text: String::new(),
            });
        }
        let pos = self.text().len();
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(pos, pos),
            text: String::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_source::SourceMap;
    use std::path::PathBuf;

    fn lex_str(text: &str) -> LexerOutput {
        let mut sm = SourceMap::new();
        let fid = sm.add(PathBuf::from("test.nx"), text.to_string());
        lex(sm.get(fid))
    }

    fn kinds(out: &LexerOutput) -> Vec<TokenKind> {
        out.tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn emits_indent_and_dedent() {
        let out = lex_str("fn main():\n    print(1)\n");
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let ks = kinds(&out);
        assert!(ks.contains(&TokenKind::Indent));
        assert!(ks.contains(&TokenKind::Dedent));
        assert_eq!(ks.last(), Some(&TokenKind::Eof));
    }

    #[test]
    fn nested_blocks_emit_matching_dedents() {
        let out = lex_str("fn main():\n    if true:\n        print(1)\n    print(2)\n");
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let ks = kinds(&out);
        let indents = ks.iter().filter(|k| **k == TokenKind::Indent).count();
        let dedents = ks.iter().filter(|k| **k == TokenKind::Dedent).count();
        assert_eq!(indents, 2);
        assert_eq!(dedents, 2);
    }

    #[test]
    fn comments_are_skipped() {
        let out = lex_str("# header\nfn main():\n    print(1) # trailing\n");
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!kinds(&out).contains(&TokenKind::Slash));
    }

    #[test]
    fn token_spans_are_monotonic() {
        let out = lex_str("fn main():\n    let x = 1\n");
        let mut last = 0usize;
        for t in &out.tokens {
            if matches!(
                t.kind,
                TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof
            ) {
                continue;
            }
            assert!(
                t.span.start >= last,
                "token {:?} at {} after {}",
                t.kind,
                t.span.start,
                last
            );
            last = t.span.start;
            assert!(t.span.end >= t.span.start);
        }
    }

    #[test]
    fn unterminated_string_reports_l002() {
        let out = lex_str("fn main():\n    let s = \"oops\n");
        assert!(out.diagnostics.iter().any(|d| d.code.0 == "L002"));
    }

    #[test]
    fn range_token_is_one_token() {
        let out = lex_str("fn main():\n    for i in 0..3:\n        print(i)\n");
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(kinds(&out).contains(&TokenKind::Range));
    }

    #[test]
    fn compound_assignment_tokens_are_single_tokens() {
        let out =
            lex_str("fn main():\n    x += 1\n    x -= 1\n    x *= 1\n    x /= 1\n    x %= 1\n");
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let ks = kinds(&out);
        for k in [
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
        ] {
            assert!(ks.contains(&k), "missing {k:?}");
        }
        // No bare `=` from the compound operators.
        assert!(!ks.contains(&TokenKind::Eq));
    }
}
