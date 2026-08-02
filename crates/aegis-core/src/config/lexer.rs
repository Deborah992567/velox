//! The configuration lexer.
//!
//! Turns configuration text into a stream of [`Token`]s, each carrying a
//! 1-based [`Pos`] (line, column). The grammar is intentionally close to the
//! well-known block/directive format:
//!
//! ```text
//! # comment
//! directive arg1 arg2;          # leaf directive
//! block {                       # block directive
//!     nested value;
//! }
//! "quoted string" 'single quoted'
//! ```
//!
//! Tokens are position-stamped so the parser and validator can report
//! `file:line:column` diagnostics for every error.

use crate::core::{Context, Error, ErrorKind, Result};

/// A 1-based source position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pos {
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
}

impl std::fmt::Display for Pos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// The kind of a [`Token`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// An unquoted word (`directive`, `80`, `http://backend`, ...).
    Word,
    /// A quoted string with escapes resolved (`"a b"`).
    String,
    /// `;`
    Semicolon,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// End of input.
    Eof,
}

/// A lexed token with its source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token kind.
    pub kind: TokenKind,
    /// The token text. For [`TokenKind::String`] escapes are resolved; for
    /// others this is the raw slice.
    pub text: String,
    /// Where the token starts.
    pub pos: Pos,
}

impl Token {
    fn word(text: &str, pos: Pos) -> Self {
        Self {
            kind: TokenKind::Word,
            text: text.to_owned(),
            pos,
        }
    }

    const fn string(text: String, pos: Pos) -> Self {
        Self {
            kind: TokenKind::String,
            text,
            pos,
        }
    }
}

struct Lexer<'a> {
    input: &'a [u8],
    index: usize,
    line: usize,
    column: usize,
}

/// Tokenize `input`, returning all tokens including a trailing
/// [`TokenKind::Eof`].
pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut lexer = Lexer {
        input: input.as_bytes(),
        index: 0,
        line: 1,
        column: 1,
    };
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        let is_eof = token.kind == TokenKind::Eof;
        tokens.push(token);
        if is_eof {
            return Ok(tokens);
        }
    }
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.index).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        if byte == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(byte)
    }

    const fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            column: self.column,
        }
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<()> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.bump();
                }
                Some(b'#') => {
                    while let Some(byte) = self.peek() {
                        if byte == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Token> {
        self.skip_whitespace_and_comments()?;
        let pos = self.pos();
        let Some(byte) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                text: String::new(),
                pos,
            });
        };

        match byte {
            b';' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::Semicolon,
                    text: ";".to_owned(),
                    pos,
                })
            }
            b'{' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::LBrace,
                    text: "{".to_owned(),
                    pos,
                })
            }
            b'}' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::RBrace,
                    text: "}".to_owned(),
                    pos,
                })
            }
            b'"' | b'\'' => self.lex_quoted(byte, pos),
            _ => self.lex_word(pos),
        }
    }

    fn lex_quoted(&mut self, quote: u8, pos: Pos) -> Result<Token> {
        self.bump(); // opening quote
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.bump() else {
                return Err(Error::new(ErrorKind::Parse, "unterminated quoted string"))
                    .with_context(ErrorKind::Parse, || format!("at {pos}"));
            };
            match byte {
                b'\\' => {
                    let Some(escaped) = self.bump() else {
                        return Err(Error::new(ErrorKind::Parse, "unterminated escape sequence"))
                            .with_context(ErrorKind::Parse, || format!("at {pos}"));
                    };
                    let resolved = match escaped {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        other => other,
                    };
                    out.push(resolved);
                }
                other if other == quote => {
                    let text = String::from_utf8(out).map_err(|_| {
                        Error::new(ErrorKind::Parse, "quoted string is not valid UTF-8")
                    })?;
                    return Ok(Token::string(text, pos));
                }
                other => out.push(other),
            }
        }
    }

    fn lex_word(&mut self, pos: Pos) -> Result<Token> {
        let start = self.index;
        while let Some(byte) = self.peek() {
            if matches!(
                byte,
                b' ' | b'\t' | b'\r' | b'\n' | b';' | b'{' | b'}' | b'"' | b'\'' | b'#'
            ) {
                break;
            }
            self.bump();
        }
        if self.index == start {
            return Err(Error::new(
                ErrorKind::Parse,
                format!("unexpected character '{}'", self.input[start] as char),
            ))
            .with_context(ErrorKind::Parse, || format!("at {pos}"));
        }
        let text = std::str::from_utf8(&self.input[start..self.index])
            .map_err(|_| Error::new(ErrorKind::Parse, "token is not valid UTF-8"))?;
        Ok(Token::word(text, pos))
    }
}

#[cfg(test)]
mod tests {
    use super::{Pos, TokenKind, tokenize};

    #[test]
    fn lexes_simple_directives() {
        let tokens = tokenize("worker_processes 4;\n").unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Semicolon,
                TokenKind::Eof
            ]
        );
        assert_eq!(tokens[0].text, "worker_processes");
        assert_eq!(tokens[1].text, "4");
    }

    #[test]
    fn lexes_blocks_and_nested_content() {
        let tokens = tokenize("events { worker_connections 100; }").unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Word,
                TokenKind::LBrace,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Semicolon,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn comments_are_skipped() {
        let tokens = tokenize("# leading comment\nlisten 80; # trailing\n").unwrap();
        assert_eq!(tokens[0].text, "listen");
        assert_eq!(tokens[1].text, "80");
    }

    #[test]
    fn quoted_strings_resolve_escapes() {
        let tokens = tokenize("proxy_pass \"http://x\\\"y\";").unwrap();
        assert_eq!(tokens[1].kind, TokenKind::String);
        assert_eq!(tokens[1].text, "http://x\"y");
    }

    #[test]
    fn positions_are_reported() {
        let tokens = tokenize("a {\n  b c;\n}\n").unwrap();
        assert_eq!(tokens[0].pos, Pos { line: 1, column: 1 });
        // 'b' starts at line 2, column 3 (two spaces of indentation).
        assert_eq!(tokens[2].pos, Pos { line: 2, column: 3 });
        // ';' is at line 2 column 6, '}' at line 3 column 1.
        assert_eq!(tokens[4].pos, Pos { line: 2, column: 6 });
        assert_eq!(tokens[5].pos, Pos { line: 3, column: 1 });
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let error = tokenize("root \"oops").unwrap_err();
        assert!(error.to_string().contains("unterminated"));
    }

    #[test]
    fn words_can_contain_punctuation() {
        let tokens = tokenize("proxy_pass http://127.0.0.1:8080/path;").unwrap();
        assert_eq!(tokens[1].text, "http://127.0.0.1:8080/path");
    }

    #[test]
    fn eof_token_is_always_last() {
        let tokens = tokenize("").unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
    }
}
