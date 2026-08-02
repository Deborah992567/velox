//! Log severity levels.
//!
//! The eight levels mirror the classic syslog/nginx severity ladder:
//! `debug`, `info`, `notice`, `warn`, `error`, `crit`, `alert`, `emerg`.
//! Ordering is by severity: `debug` is the least severe, `emerg` the most.

use std::fmt;
use std::str::FromStr;

use crate::core::{Error, ErrorKind, Result};

/// A log severity level.
///
/// Variants are ordered so that `Debug < Info < Notice < Warn < Error <
/// Crit < Alert < Emerg`. A logger with a configured minimum level drops
/// records whose severity is below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Level {
    /// Fine-grained diagnostics, typically enabled only during development.
    Debug,
    /// Normal operational messages.
    Info,
    /// Notable but normal events.
    Notice,
    /// Potentially harmful situations.
    Warn,
    /// Errors that a request/operation recovers from.
    Error,
    /// Critical conditions; may affect service availability.
    Crit,
    /// Immediate action required.
    Alert,
    /// System is unusable.
    Emerg,
}

impl Level {
    /// The lowercase string form used in config files and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Crit => "crit",
            Self::Alert => "alert",
            Self::Emerg => "emerg",
        }
    }

    /// The syslog integer priority (0 = emerg .. 7 = debug).
    #[must_use]
    pub const fn as_syslog_priority(self) -> u8 {
        match self {
            Self::Emerg => 0,
            Self::Alert => 1,
            Self::Crit => 2,
            Self::Error => 3,
            Self::Warn => 4,
            Self::Notice => 5,
            Self::Info => 6,
            Self::Debug => 7,
        }
    }
}

impl FromStr for Level {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "notice" => Ok(Self::Notice),
            "warn" | "warning" => Ok(Self::Warn),
            "error" | "err" => Ok(Self::Error),
            "crit" | "critical" => Ok(Self::Crit),
            "alert" => Ok(Self::Alert),
            "emerg" | "emergency" => Ok(Self::Emerg),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("unknown log level '{other}'"),
            )),
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse a level name from configuration, with a context label for errors.
#[must_use]
pub fn parse_level(value: &str) -> Level {
    Level::from_str(value).unwrap_or(Level::Error)
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn levels_parse_and_roundtrip() {
        for level in [
            Level::Debug,
            Level::Info,
            Level::Notice,
            Level::Warn,
            Level::Error,
            Level::Crit,
            Level::Alert,
            Level::Emerg,
        ] {
            let parsed: Level = level.as_str().parse().unwrap();
            assert_eq!(parsed, level);
            assert_eq!(parsed.to_string(), level.as_str());
        }
    }

    #[test]
    fn case_and_aliases_are_accepted() {
        assert_eq!("DEBUG".parse::<Level>().unwrap(), Level::Debug);
        assert_eq!("warning".parse::<Level>().unwrap(), Level::Warn);
        assert_eq!("emergency".parse::<Level>().unwrap(), Level::Emerg);
    }

    #[test]
    fn unknown_level_is_rejected() {
        assert!("verbose".parse::<Level>().is_err());
    }

    #[test]
    fn ordering_follows_severity() {
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Crit);
        assert!(Level::Crit < Level::Alert);
        assert!(Level::Alert < Level::Emerg);
    }

    #[test]
    fn syslog_priorities_are_7_minus_index() {
        assert_eq!(Level::Emerg.as_syslog_priority(), 0);
        assert_eq!(Level::Debug.as_syslog_priority(), 7);
    }
}
