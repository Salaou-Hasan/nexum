//! Lexer tests: token shapes, escapes, numbers, comments and blocks.

use crate::{lex, TokenKind};

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
fn no_indent_or_dedent_inside_brackets() {
    // A bracket left open at a line break used to emit an Indent on the
    // continuation line, so every multi-line list, call or dict literal
    // was a parse error. The Indent/Dedent channel is silent while any
    // bracket is open; an actual block still emits them (see
    // `indent_dedent`).
    let k = kinds("a = [\n    1,\n    2,\n]\nprint(a)");
    assert!(
        !k.iter()
            .any(|t| matches!(t, TokenKind::Indent | TokenKind::Dedent)),
        "Indent/Dedent must not be emitted inside brackets: {k:?}"
    );
    assert_eq!(k.first(), Some(&TokenKind::Ident));
    assert!(k.contains(&TokenKind::LBracket));
    assert!(k.contains(&TokenKind::RBracket));
    assert_eq!(k.last(), Some(&TokenKind::Eof));
}

#[test]
fn bad_char_errors() {
    assert!(lex("$").is_err());
}

/// `@` is the matrix-multiplication operator, so it lexes as a token
/// rather than erroring like the remaining bad characters.
#[test]
fn at_lexes_as_matmul() {
    assert_eq!(kinds("@"), vec![TokenKind::At, TokenKind::Eof]);
    assert_eq!(kinds("@="), vec![TokenKind::AtEq, TokenKind::Eof]);
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
