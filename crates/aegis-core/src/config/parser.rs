//! The configuration parser.
//!
//! Recursive-descent parser over the token stream produced by the lexer.
//! Grammar:
//!
//! ```text
//! config     := directive*
//! directive  := NAME arg* (';' | block)
//! block      := '{' directive* '}'
//! ```
//!
//! Block directives do not require a trailing `;` (matching the format used
//! for `http`, `server`, `location`, ...). Every syntax error reports the
//! offending `line:column`.

use crate::config::ast::{ConfigNode, ConfigRoot};
use crate::config::lexer::{Pos, Token, TokenKind, tokenize};
use crate::core::{Error, ErrorKind, Result, SourcePos};

/// Parse configuration text into an AST.
///
/// Positions carry `file: "config"`; use [`parse_named`] to report a real
/// file path in diagnostics.
pub fn parse(input: &str) -> Result<ConfigRoot> {
    parse_named(input, "config")
}

/// Parse configuration text, tagging all diagnostics with `file`.
pub fn parse_named(input: &str, file: &str) -> Result<ConfigRoot> {
    let tokens = tokenize(input)?;
    let mut parser = Parser {
        tokens,
        index: 0,
        file: file.to_owned(),
    };
    let nodes = parser.parse_directives(false)?;
    parser.expect(TokenKind::Eof)?;
    Ok(ConfigRoot { nodes })
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    file: String,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
        token
    }

    fn error_at(&self, pos: Pos, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Parse, message).with_position(SourcePos {
            file: self.file.clone(),
            line: pos.line,
            column: pos.column,
        })
    }

    /// Parse a run of directives, stopping at end of file and (when
    /// `stop_at_rbrace` is set) at a closing `}`. The closing brace is not
    /// consumed here; the caller consumes it with `expect`.
    fn parse_directives(&mut self, stop_at_rbrace: bool) -> Result<Vec<ConfigNode>> {
        let mut nodes = Vec::new();
        loop {
            let kind = self.peek().kind;
            match kind {
                TokenKind::Eof => break,
                TokenKind::RBrace if stop_at_rbrace => break,
                _ => nodes.push(self.parse_directive()?),
            }
        }
        Ok(nodes)
    }

    fn parse_directive(&mut self) -> Result<ConfigNode> {
        let name_token = self.advance();
        if name_token.kind != TokenKind::Word {
            return Err(self.error_at(name_token.pos, "expected a directive name"));
        }

        let mut args = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::Semicolon => {
                    self.advance();
                    return Ok(ConfigNode::leaf(name_token.text, args, name_token.pos));
                }
                TokenKind::LBrace => {
                    self.advance();
                    let children = self.parse_directives(true)?;
                    self.expect(TokenKind::RBrace)?;
                    return Ok(ConfigNode::block(
                        name_token.text,
                        args,
                        children,
                        name_token.pos,
                    ));
                }
                TokenKind::Word | TokenKind::String => {
                    args.push(self.advance().text);
                }
                TokenKind::RBrace => {
                    return Err(self.error_at(
                        self.peek().pos,
                        format!("unexpected '}}', expected ';' after '{}'", name_token.text),
                    ));
                }
                TokenKind::Eof => {
                    return Err(self.error_at(
                        name_token.pos,
                        format!(
                            "unexpected end of file, directive '{}' is incomplete",
                            name_token.text
                        ),
                    ));
                }
            }
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            let marker = kind_marker(kind);
            let message = if self.peek().kind == TokenKind::Eof {
                format!("unexpected end of file, expected '{marker}'")
            } else {
                format!("expected '{marker}'")
            };
            Err(self.error_at(self.peek().pos, message))
        }
    }
}

const fn kind_marker(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Semicolon => ";",
        TokenKind::LBrace => "{",
        TokenKind::RBrace => "}",
        TokenKind::Eof => "end of file",
        TokenKind::Word | TokenKind::String => "token",
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_leaf_and_block_directives() {
        let root = parse(
            "worker_processes 4;\n\
             events {\n\
                 worker_connections 10000;\n\
             }\n\
             http {\n\
                 server {\n\
                     listen 80;\n\
                 }\n\
             }\n",
        )
        .unwrap();
        assert_eq!(root.nodes.len(), 3);
        assert_eq!(root.nodes[0].name, "worker_processes");
        assert_eq!(root.nodes[0].args, vec!["4"]);
        assert!(root.nodes[0].children.is_empty());

        let events = &root.nodes[1];
        assert_eq!(events.name, "events");
        assert!(events.is_block());
        assert_eq!(events.children[0].name, "worker_connections");

        let http = &root.nodes[2];
        let server = &http.children[0];
        assert_eq!(server.name, "server");
        assert_eq!(server.children[0].name, "listen");
        assert_eq!(server.children[0].args, vec!["80"]);
    }

    #[test]
    fn parses_location_with_args_and_body() {
        let root = parse(
            "location /api/ {\n\
                 proxy_pass http://backend;\n\
             }\n",
        )
        .unwrap();
        let location = &root.nodes[0];
        assert_eq!(location.name, "location");
        assert_eq!(location.args, vec!["/api/"]);
        assert_eq!(location.children[0].name, "proxy_pass");
        assert_eq!(location.children[0].args, vec!["http://backend"]);
    }

    #[test]
    fn quoted_args_are_supported() {
        let root = parse("server_name \"www.example.com\";\n").unwrap();
        assert_eq!(root.nodes[0].args, vec!["www.example.com"]);
    }

    #[test]
    fn missing_semicolon_is_reported_with_position() {
        let error = parse("events {\n  worker_connections 10000\n}\n").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("expected ';'"), "{text}");
        assert!(text.contains(":3:1"), "{text}");
    }

    #[test]
    fn unterminated_block_is_an_error() {
        let error = parse("http {\n  server {\n").unwrap_err();
        assert!(error.to_string().contains("unexpected end of file"));
    }

    #[test]
    fn stray_closing_brace_is_an_error() {
        let error = parse("}\n").unwrap_err();
        assert!(error.to_string().contains("expected a directive name"));
    }

    #[test]
    fn empty_config_parses() {
        let root = parse("").unwrap();
        assert!(root.nodes.is_empty());
    }
}
