//! The log record: a single structured log message.
//!
//! A [`LogRecord`] is produced by [`crate::logging::log_at`] (or the
//! [`crate::log!`] family of macros) and consumed by [`LogSink`]s. It is
//! intentionally format-agnostic: text rendering and JSON serialization both
//! consume the same record so structured and plain logs cannot drift apart.

use std::fmt;

use time::OffsetDateTime;

use super::Level;

/// A single log event.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Time of the event, in UTC.
    pub time: OffsetDateTime,
    /// Severity level.
    pub level: Level,
    /// Module path that produced the record.
    pub target: &'static str,
    /// The formatted message.
    pub message: String,
    /// Optional structured key/value fields (used by JSON sinks).
    pub fields: Vec<(String, String)>,
}

impl LogRecord {
    /// Construct a record with the given time.
    #[must_use]
    pub fn new(
        time: OffsetDateTime,
        level: Level,
        target: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            time,
            level,
            target,
            message: message.into(),
            fields: Vec::new(),
        }
    }

    /// Construct a record stamped with the current UTC time.
    #[must_use]
    pub fn now(level: Level, target: &'static str, message: impl Into<String>) -> Self {
        Self::new(OffsetDateTime::now_utc(), level, target, message)
    }

    /// Append a structured field.
    #[must_use]
    pub fn field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), value.into()));
        self
    }

    /// Attach a list of structured fields.
    #[must_use]
    pub fn fields<I, K, V>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.fields
            .extend(values.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }
}

impl fmt::Display for LogRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] {}: {}",
            self.time, self.level, self.target, self.message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::LogRecord;
    use crate::logging::level::Level;
    use time::macros::datetime;

    #[test]
    fn record_carries_timestamp_level_and_target() {
        let record = LogRecord::new(
            datetime!(2026-08-02 12:00:00 UTC),
            Level::Warn,
            "aegis::test",
            "something odd",
        );
        assert_eq!(record.level, Level::Warn);
        assert_eq!(record.target, "aegis::test");
        assert_eq!(record.message, "something odd");
        assert_eq!(
            record.to_string(),
            "2026-08-02 12:00:00.0 +00:00:00 [warn] aegis::test: something odd"
        );
    }

    #[test]
    fn structured_fields_accumulate() {
        let record = LogRecord::now(Level::Info, "aegis::test", "hi")
            .field("code", "404")
            .fields([("method", "GET"), ("uri", "/x")]);
        assert_eq!(record.fields.len(), 3);
        assert_eq!(record.fields[0], ("code".to_string(), "404".to_string()));
    }

    #[test]
    fn now_stamps_current_time() {
        let before = time::OffsetDateTime::now_utc();
        let record = LogRecord::now(Level::Debug, "aegis::test", "x");
        let after = time::OffsetDateTime::now_utc();
        assert!(record.time >= before && record.time <= after);
    }
}
