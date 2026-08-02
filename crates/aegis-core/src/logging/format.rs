//! Rendering of [`LogRecord`]s into text or JSON.
//!
//! Both formats consume the same [`LogRecord`]; JSON output carries the
//! structured fields as an object, text output is human-readable and includes
//! timestamp, level, and target.

use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::LogRecord;

/// The rendering format for a log sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// `2026-08-02T12:00:00Z [info] target: message` style lines.
    Text,
    /// One JSON object per line.
    Json,
}

/// Render an RFC 3339 timestamp, falling back to a debug string if the
/// system clock is out of range (essentially never).
#[must_use]
pub fn format_timestamp(time: OffsetDateTime) -> String {
    time.format(&Rfc3339).unwrap_or_else(|_| time.to_string())
}

/// Render a record in the configured format.
#[must_use]
pub fn render(format: LogFormat, record: &LogRecord) -> String {
    match format {
        LogFormat::Text => render_text(record),
        LogFormat::Json => render_json(record),
    }
}

fn render_text(record: &LogRecord) -> String {
    if record.fields.is_empty() {
        format!(
            "{} [{}] {}: {}",
            format_timestamp(record.time),
            record.level,
            record.target,
            record.message
        )
    } else {
        let fields = record
            .fields
            .iter()
            .map(|(k, v)| format!("{k}=\"{v}\""))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{} [{}] {}: {} ({fields})",
            format_timestamp(record.time),
            record.level,
            record.target,
            record.message
        )
    }
}

fn render_json(record: &LogRecord) -> String {
    let fields: Value = if record.fields.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        let map = record
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        Value::Object(map)
    };
    let value = json!({
        "timestamp": format_timestamp(record.time),
        "level": record.level.as_str(),
        "target": record.target,
        "message": record.message,
        "fields": fields,
    });
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{render, LogFormat};
    use crate::logging::Level;
    use crate::logging::LogRecord;
    use time::macros::datetime;

    fn record() -> LogRecord {
        LogRecord::new(
            datetime!(2026-08-02 12:00:00 UTC),
            Level::Info,
            "aegis::test",
            "hello",
        )
        .field("code", "200")
    }

    #[test]
    fn text_format_is_human_readable() {
        let line = render(LogFormat::Text, &record());
        assert_eq!(
            line,
            "2026-08-02T12:00:00Z [info] aegis::test: hello (code=\"200\")"
        );
    }

    #[test]
    fn text_format_omits_fields_when_empty() {
        let line = render(LogFormat::Text, &record());
        assert!(line.contains("code=\"200\""));
        let bare = LogRecord::new(
            datetime!(2026-08-02 12:00:00 UTC),
            Level::Info,
            "aegis::test",
            "hello",
        );
        assert!(!render(LogFormat::Text, &bare).contains('('));
    }

    #[test]
    fn json_format_is_parseable_and_complete() {
        let line = render(LogFormat::Json, &record());
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["level"], "info");
        assert_eq!(parsed["target"], "aegis::test");
        assert_eq!(parsed["message"], "hello");
        assert_eq!(parsed["fields"]["code"], "200");
        assert_eq!(parsed["timestamp"], "2026-08-02T12:00:00Z");
    }

    #[test]
    fn timestamp_is_rfc3339() {
        let stamp = super::format_timestamp(datetime!(2026-08-02 12:00:00 UTC));
        assert_eq!(stamp, "2026-08-02T12:00:00Z");
    }
}
