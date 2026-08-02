//! Logging subsystem: levels, records, formats, sinks, and the logger.
//!
//! Entry points:
//!
//! * [`Logger`] / [`set_global`] / [`init_global`] — configure the process.
//! * [`log_at`] and the [`crate::log!`] macros — emit records.
//! * [`LogSink`], [`FileSink`], [`StreamSink`], [`NullSink`], [`sink_for`] —
//!   destinations with buffering and rotation support.
//! * [`LogFormat`] — text or JSON rendering.

pub mod format;
pub mod level;
pub mod logger;
pub mod record;
pub mod sink;

pub use format::LogFormat;
pub use level::Level;
pub use logger::{Logger, flush, init_global, is_enabled, log_at, reopen, set_global, with_global};
pub use record::LogRecord;
pub use sink::{FileSink, LogSink, NullSink, StreamSink, sink_for};
