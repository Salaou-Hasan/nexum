//! Token types: the vocabulary of the Nexum language.
//!
//! `TokenKind` lists every terminal the parser can see, `keyword_kind`
//! maps a lexeme to its keyword, and `Token` / `LexError` carry positions.

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
    At,
    AtEq,
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

pub(crate) fn keyword_kind(lexeme: &str) -> Option<TokenKind> {
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
