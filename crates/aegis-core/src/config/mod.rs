//! Configuration subsystem: lexer, parser, AST, and validator.
//!
//! The pipeline is:
//!
//! ```text
//! text ──▶ lexer (tokens + positions) ──▶ parser (AST) ──▶ validator ──▶ runtime config
//! ```
//!
//! Phase 1 provides the lexer, parser, AST, and a structural validator (the
//! runtime typed configuration arrives with routing in Phase 7). Every
//! diagnostic carries `file:line:column`.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod validator;
pub mod watcher;

pub use ast::{ConfigNode, ConfigRoot};
pub use lexer::{Pos, Token, TokenKind, tokenize};
pub use parser::{parse, parse_named};
pub use validator::{ConfigValidator, validate};
pub use watcher::{ConfigWatcher, ReloadEvent, ReloadInfo, ReloadPolicy};
