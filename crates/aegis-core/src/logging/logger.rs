//! The logger: level filtering, sink fan-out, and a process-global instance.
//!
//! A [`Logger`] holds an ordered list of [`LogSink`]s and a minimum
//! [`Level`]. Records below the minimum are dropped before any sink is
//! touched. The [`LOGGER`] global is configured once at startup (e.g. from
//! the `error_log` directive) and used by the [`crate::log!`] family of
//! macros and by [`log_at`].
//!
//! Logging is not on the request hot path by default: per-request access
//! logging will use dedicated fast paths in later phases.

use std::sync::Mutex;
use std::sync::OnceLock;

use super::Level;
use super::LogRecord;
use super::LogSink;

/// The process-global logger.
static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

/// A configured logging pipeline.
pub struct Logger {
    sinks: Vec<Box<dyn LogSink>>,
    min_level: Level,
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logger")
            .field("sinks", &self.sinks.len())
            .field("min_level", &self.min_level)
            .finish()
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self {
            sinks: Vec::new(),
            min_level: Level::Notice,
        }
    }
}

impl Logger {
    /// Create an empty logger with the given minimum level.
    #[must_use]
    pub fn new(min_level: Level) -> Self {
        Self {
            sinks: Vec::new(),
            min_level,
        }
    }

    /// The configured minimum level.
    #[must_use]
    pub const fn min_level(&self) -> Level {
        self.min_level
    }

    /// Add a sink. Sinks are written in insertion order.
    pub fn add_sink(&mut self, sink: Box<dyn LogSink>) {
        self.sinks.push(sink);
    }

    /// Whether this logger would emit a record at the given level.
    #[must_use]
    pub fn enabled(&self, level: Level) -> bool {
        level >= self.min_level
    }

    /// Write a record to every sink that passes the level filter.
    ///
    /// Sink failures are reported to stderr so a broken log destination
    /// cannot take down request processing.
    pub fn log(&mut self, record: &LogRecord) {
        if !self.enabled(record.level) {
            return;
        }
        for sink in &mut self.sinks {
            if let Err(error) = sink.write(record) {
                eprintln!("aegis: log write failed: {error}");
            }
        }
    }

    /// Flush all sinks.
    pub fn flush(&mut self) {
        for sink in &mut self.sinks {
            if let Err(error) = sink.flush() {
                eprintln!("aegis: log flush failed: {error}");
            }
        }
    }

    /// Reopen all sinks (log rotation).
    pub fn reopen(&mut self) {
        for sink in &mut self.sinks {
            if let Err(error) = sink.reopen() {
                eprintln!("aegis: log reopen failed: {error}");
            }
        }
    }
}

/// Install the process-global logger.
///
/// The global can be set at most once per process (workers and the master
/// each configure their own logger at startup).
pub fn set_global(logger: Logger) -> Result<(), Mutex<Logger>> {
    LOGGER.set(Mutex::new(logger))
}

/// Install the global logger, panicking if it is already set. For use at
/// process startup where a second initialization is a programming error.
///
/// # Panics
///
/// Panics if a global logger is already installed.
pub fn init_global(logger: Logger) {
    set_global(logger).expect("global logger already initialized");
}

/// Run `f` with the global logger, if one has been installed.
pub fn with_global<T>(f: impl FnOnce(&mut Logger) -> T) -> Option<T> {
    let logger = LOGGER.get()?;
    let mut guard = logger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Some(f(&mut guard))
}

/// Whether the global logger would emit a record at `level`.
///
/// Returns `false` if no global logger is installed.
#[must_use]
pub fn is_enabled(level: Level) -> bool {
    with_global(|logger| logger.enabled(level)).unwrap_or(false)
}

/// Log a record at the given level to the global logger, if installed.
pub fn log_at(level: Level, target: &'static str, message: String) {
    with_global(|logger| logger.log(&LogRecord::now(level, target, message)));
}

/// Flush the global logger.
pub fn flush() {
    with_global(Logger::flush);
}

/// Reopen all sinks on the global logger (rotation).
pub fn reopen() {
    with_global(Logger::reopen);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::{Logger, set_global};
    use crate::logging::{Level, LogFormat, LogRecord};
    use tempfile::tempdir;
    use time::macros::datetime;

    /// A test sink capturing rendered lines in memory.
    #[derive(Debug)]
    struct CaptureSink(Arc<Mutex<Vec<String>>>);

    impl crate::logging::LogSink for CaptureSink {
        fn write(&mut self, record: &LogRecord) -> std::io::Result<()> {
            self.0.lock().unwrap().push(record.message.clone());
            Ok(())
        }
    }

    fn record(level: Level, message: &str) -> LogRecord {
        LogRecord::new(
            datetime!(2026-08-02 12:00:00 UTC),
            level,
            "aegis::test",
            message,
        )
    }

    #[test]
    // clippy wants the guard scoped tighter, but it already ends the test.
    #[allow(clippy::significant_drop_tightening)]
    fn level_filter_drops_below_minimum() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut logger = Logger::new(Level::Warn);
        logger.add_sink(Box::new(CaptureSink(captured.clone())));

        logger.log(&record(Level::Info, "dropped"));
        logger.log(&record(Level::Warn, "kept"));
        logger.log(&record(Level::Error, "kept-too"));

        let lines = captured.lock().unwrap();
        assert_eq!(&lines[..], &["kept", "kept-too"]);
    }

    #[test]
    fn global_logger_is_installed_once() {
        let first = Logger::new(Level::Info);
        assert!(set_global(first).is_ok());
        let second = Logger::new(Level::Debug);
        assert!(set_global(second).is_err());
    }

    #[test]
    fn file_sink_via_logger_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("err.log");
        let mut logger = Logger::new(Level::Debug);
        logger.add_sink(Box::new(
            crate::logging::FileSink::open(&path, LogFormat::Text, false).unwrap(),
        ));
        logger.log(&record(Level::Error, "disk full soon"));
        logger.flush();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("disk full soon"));
    }
}
