//! Error types for the Aegis core.
//!
//! All fallible operations in the core return [`Result<T>`], an alias for
//! `std::result::Result<T, Error>`. The [`Error`] type carries:
//!
//! * a [`ErrorKind`] classifying the failure,
//! * a human-readable message,
//! * an optional source error for chaining,
//! * an optional source position (file/line/column) used for configuration
//!   diagnostics so that `aegis -t` can point at the offending directive.
//!
//! Configuration errors should be constructed with
//! [`Error::config_at`] (or [`Context::with_position`]) so they report the
//! exact `file:line:column`.

use std::fmt;
use std::io;

/// The result type used throughout the core.
pub type Result<T> = std::result::Result<T, Error>;

/// High-level classification of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Underlying OS / I/O failure.
    Io,
    /// Invalid or inconsistent configuration.
    Config,
    /// Syntax error in a parsed input (config, HTTP, protocols).
    Parse,
    /// Protocol-level violation or malformed message.
    Protocol,
    /// A configured limit was exceeded (headers, body, connections).
    Limit,
    /// An operation timed out.
    Timeout,
    /// TLS negotiation or certificate failure.
    Tls,
    /// Upstream/backend failure.
    Upstream,
    /// Cache backend failure.
    Cache,
    /// A security policy was triggered (traversal, injection, smuggling).
    Security,
    /// Programming error: invariant violated.
    Internal,
    /// Invalid client-supplied input that does not match another category.
    InvalidInput,
}

impl ErrorKind {
    /// Short stable identifier for logs and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Config => "config",
            Self::Parse => "parse",
            Self::Protocol => "protocol",
            Self::Limit => "limit",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Upstream => "upstream",
            Self::Cache => "cache",
            Self::Security => "security",
            Self::Internal => "internal",
            Self::InvalidInput => "invalid_input",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A source position in a file, 1-based line and column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourcePos {
    /// File path the position refers to.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
}

impl fmt::Display for SourcePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// The core error type.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    position: Option<SourcePos>,
}

impl Error {
    /// Construct a new error of the given kind and message.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            position: None,
        }
    }

    /// Convenience constructor for I/O errors.
    #[must_use]
    pub fn io(error: io::Error) -> Self {
        let kind = if error.kind() == io::ErrorKind::NotFound {
            Self::new(ErrorKind::Io, "no such file or directory")
        } else {
            Self::new(ErrorKind::Io, error.to_string())
        };
        kind.with_source(error)
    }

    /// Convenience constructor for configuration errors.
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    /// Convenience constructor for parse errors.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Parse, message)
    }

    /// Convenience constructor for internal-invariant violations.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// Attach a source error, returning `self`.
    #[must_use]
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attach a source position, returning `self`.
    #[must_use]
    pub fn with_position(mut self, position: SourcePos) -> Self {
        self.position = Some(position);
        self
    }

    /// Build a configuration error carrying a source position.
    #[must_use]
    pub fn config_at(position: SourcePos, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message).with_position(position)
    }

    /// The error kind.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The optional source position.
    #[must_use]
    pub const fn position(&self) -> Option<&SourcePos> {
        self.position.as_ref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.position, &self.source) {
            (Some(pos), Some(source)) => {
                write!(f, "{} in {}: {}", self.message, pos, source)
            }
            (Some(pos), None) => write!(f, "{} in {}", self.message, pos),
            (None, Some(source)) => write!(f, "{}: {}", self.message, source),
            (None, None) => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::io(value)
    }
}

impl From<ErrorKind> for Error {
    fn from(value: ErrorKind) -> Self {
        Self::new(value, value.as_str())
    }
}

/// Extension trait adding context to results and options.
///
/// Mirrors the `anyhow`/`thiserror` ergonomics without the dependency, and
/// lets call sites attach an [`ErrorKind`] and message to a failure.
pub trait Context<T> {
    /// Wrap the error in additional context with the given kind and message.
    fn context(self, kind: ErrorKind, message: impl Into<String>) -> Result<T>;

    /// Wrap the error with a lazily-computed message.
    fn with_context(self, kind: ErrorKind, message: impl FnOnce() -> String) -> Result<T>;

    /// Wrap the error in additional context, using a fallback kind for
    /// bare `Option::None`.
    fn context_or(self, kind: ErrorKind, message: impl Into<String>) -> Result<T>;

    /// Attach a source position to an already-formed error.
    fn with_position(self, position: SourcePos) -> Result<T>;
}

impl<T> Context<T> for Option<T> {
    fn context(self, kind: ErrorKind, message: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| Error::new(kind, message))
    }

    fn with_context(self, kind: ErrorKind, message: impl FnOnce() -> String) -> Result<T> {
        self.ok_or_else(|| Error::new(kind, message()))
    }

    fn context_or(self, kind: ErrorKind, message: impl Into<String>) -> Result<T> {
        self.context(kind, message)
    }

    fn with_position(self, _position: SourcePos) -> Result<T> {
        self.context(ErrorKind::Internal, "position attached to a bare Option")
    }
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, kind: ErrorKind, message: impl Into<String>) -> Result<T> {
        self.map_err(|error| Error::new(kind, message).with_source(error.into()))
    }

    fn with_context(self, kind: ErrorKind, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|error| Error::new(kind, message()).with_source(error.into()))
    }

    fn context_or(self, kind: ErrorKind, message: impl Into<String>) -> Result<T> {
        self.context(kind, message)
    }

    fn with_position(self, position: SourcePos) -> Result<T> {
        self.map_err(|error| error.into().with_position(position))
    }
}

#[cfg(test)]
mod tests {
    use super::{Context, Error, ErrorKind, Result, SourcePos};

    fn fail() -> Result<()> {
        Err(Error::config("boom"))
    }

    #[test]
    fn kind_and_message_are_exposed() {
        let error = fail().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert_eq!(error.message(), "boom");
    }

    #[test]
    fn display_omits_position_when_absent() {
        assert_eq!(fail().unwrap_err().to_string(), "boom");
    }

    #[test]
    fn display_includes_position_when_present() {
        let pos = SourcePos {
            file: "aegis.conf".into(),
            line: 12,
            column: 5,
        };
        let error = Error::config_at(pos.clone(), "bad directive");
        assert_eq!(error.to_string(), "bad directive in aegis.conf:12:5");
        assert_eq!(error.position(), Some(&pos));
    }

    #[test]
    fn io_errors_convert_with_kind() {
        let io_error = std::io::Error::from(std::io::ErrorKind::NotFound);
        let error: Error = io_error.into();
        assert_eq!(error.kind(), ErrorKind::Io);
    }

    #[test]
    fn context_wraps_result_errors() {
        let error = fail().context(ErrorKind::Config, "while loading config");
        assert_eq!(error.unwrap_err().message(), "while loading config");
    }

    #[test]
    fn context_wraps_option_none() {
        let value: Option<u32> = None;
        let error = value.context(ErrorKind::Limit, "missing value");
        assert_eq!(error.unwrap_err().kind(), ErrorKind::Limit);
    }

    #[test]
    fn error_source_chain_is_exposed() {
        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let error = Error::new(ErrorKind::Io, "open failed").with_source(inner);
        assert!(std::error::Error::source(&error).is_some());
    }
}
