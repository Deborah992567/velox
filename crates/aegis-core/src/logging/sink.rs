//! Log sinks: destinations that receive formatted [`LogRecord`]s.
//!
//! [`LogSink`] is the pluggable output boundary. Built-ins are
//! [`StreamSink`] (stdout/stderr) and [`FileSink`]; a file sink supports
//! buffered writes and a [`LogSink::reopen`] hook for log rotation (the
//! master sends `SIGUSR1` to workers to reopen files after `logrotate`).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::LogRecord;
use super::format::{LogFormat, render};
use crate::core::{Error, Result};

/// A destination for log records.
///
/// Sinks must be `Send`; rendering happens before the sink is called, so
/// sinks never see formatting concerns. The global logger holds a mutex, so
/// a sink may be driven from whichever process owns logging.
pub trait LogSink: Send {
    /// Write one rendered record.
    fn write(&mut self, record: &LogRecord) -> io::Result<()>;

    /// Flush any buffered output. The default is a no-op.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Reopen the underlying destination (log rotation). The default is a
    /// no-op.
    fn reopen(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A sink that writes rendered lines to an arbitrary stream (stdout/stderr).
pub struct StreamSink {
    format: LogFormat,
    out: Box<dyn Write + Send>,
}

impl std::fmt::Debug for StreamSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSink")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl StreamSink {
    /// A sink writing to standard output.
    #[must_use]
    pub fn stdout(format: LogFormat) -> Self {
        Self {
            format,
            out: Box::new(io::stdout()),
        }
    }

    /// A sink writing to standard error.
    #[must_use]
    pub fn stderr(format: LogFormat) -> Self {
        Self {
            format,
            out: Box::new(io::stderr()),
        }
    }
}

impl LogSink for StreamSink {
    fn write(&mut self, record: &LogRecord) -> io::Result<()> {
        self.out.write_all(render(self.format, record).as_bytes())?;
        self.out.write_all(b"\n")
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// A sink that appends to a file. Supports buffering and reopen for rotation.
#[derive(Debug)]
pub struct FileSink {
    format: LogFormat,
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    autoflush: bool,
}

impl FileSink {
    /// Open a file sink in append mode.
    ///
    /// When `buffered` is true, output is accumulated in a `BufWriter` and
    /// flushed only on [`LogSink::flush`] or when the buffer fills; when
    /// false, every record is flushed immediately.
    pub fn open(path: impl AsRef<Path>, format: LogFormat, buffered: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())
            .map_err(Error::io)?;
        Ok(Self {
            format,
            path: path.as_ref().to_path_buf(),
            writer: Some(BufWriter::new(file)),
            autoflush: !buffered,
        })
    }

    /// The path this sink writes to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether records are flushed after every write.
    #[must_use]
    pub const fn is_autoflush(&self) -> bool {
        self.autoflush
    }
}

impl LogSink for FileSink {
    fn write(&mut self, record: &LogRecord) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "sink is closed"))?;
        writer.write_all(render(self.format, record).as_bytes())?;
        writer.write_all(b"\n")?;
        if self.autoflush {
            writer.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }

    fn reopen(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }
}

/// A sink that discards everything (e.g. `error_log off`).
#[derive(Debug)]
pub struct NullSink;

impl LogSink for NullSink {
    fn write(&mut self, _record: &LogRecord) -> io::Result<()> {
        Ok(())
    }
}

/// Create a sink from a log destination string.
///
/// Accepts `stderr`, `stdout`, `off`, `/dev/null`, or a file path.
pub fn sink_for(destination: &str, format: LogFormat, buffered: bool) -> Result<Box<dyn LogSink>> {
    let boxed: Box<dyn LogSink> = match destination {
        "stderr" => Box::new(StreamSink::stderr(format)),
        "stdout" => Box::new(StreamSink::stdout(format)),
        "off" | "/dev/null" | "null" => Box::new(NullSink),
        path => Box::new(FileSink::open(path, format, buffered)?),
    };
    Ok(boxed)
}

#[cfg(test)]
mod tests {
    use super::{FileSink, LogSink, StreamSink, sink_for};
    use crate::logging::{Level, LogFormat, LogRecord};
    use tempfile::tempdir;
    use time::macros::datetime;

    fn record() -> LogRecord {
        LogRecord::new(
            datetime!(2026-08-02 12:00:00 UTC),
            Level::Warn,
            "aegis::test",
            "boom",
        )
    }

    #[test]
    fn stream_sinks_render_and_flush() {
        let mut out = StreamSink::stdout(LogFormat::Text);
        out.write(&record()).unwrap();
        out.flush().unwrap();
        let mut err = StreamSink::stderr(LogFormat::Json);
        err.write(&record()).unwrap();
        err.flush().unwrap();
    }

    #[test]
    fn file_sink_appends_buffered_and_unbuffered() {
        let dir = tempdir().unwrap();
        for buffered in [true, false] {
            let path = dir.path().join(format!("log-{buffered}.log"));
            {
                let mut sink = FileSink::open(&path, LogFormat::Text, buffered).unwrap();
                assert_eq!(sink.is_autoflush(), !buffered);
                sink.write(&record()).unwrap();
                sink.flush().unwrap();
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            assert!(
                contents.contains("aegis::test: boom"),
                "buffered={buffered} contents={contents:?}"
            );
        }
    }

    #[test]
    fn file_sink_reopen_supports_rotation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("access.log");
        {
            let mut sink = FileSink::open(&path, LogFormat::Json, true).unwrap();
            sink.write(&record()).unwrap();
            sink.flush().unwrap();

            std::fs::rename(&path, dir.path().join("access.log.1")).unwrap();
            sink.reopen().unwrap();

            sink.write(&record().field("after", "rotate")).unwrap();
            sink.flush().unwrap();
        }
        let rotated = std::fs::read_to_string(dir.path().join("access.log.1")).unwrap();
        assert!(rotated.contains("boom"));
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("after"));
    }

    #[test]
    fn sink_for_accepts_known_destinations() {
        assert!(sink_for("stderr", LogFormat::Text, true).is_ok());
        assert!(sink_for("stdout", LogFormat::Text, true).is_ok());
        assert!(sink_for("off", LogFormat::Text, true).is_ok());
        assert!(sink_for("/dev/null", LogFormat::Text, true).is_ok());
    }
}
