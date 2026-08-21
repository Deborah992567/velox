//! Access log formatter.
//!
//! Produces structured access log lines following common log format (CLF)
//! and combined log format patterns.

use std::fmt::Write;
use std::time::Duration;

/// HTTP method for access logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogMethod(pub String);

impl std::fmt::Display for LogMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Access log entry representing one completed request.
#[derive(Debug, Clone)]
pub struct AccessLogEntry {
    pub remote_addr: String,
    pub method: LogMethod,
    pub uri: String,
    pub status: u16,
    pub body_bytes: u64,
    pub duration: Duration,
    pub referer: String,
    pub user_agent: String,
    pub upstream_addr: Option<String>,
    pub request_id: Option<String>,
}

impl AccessLogEntry {
    pub fn format_combined(&self) -> String {
        let duration_ms = self.duration.as_millis();
        let upstream = self.upstream_addr.as_deref().unwrap_or("-");
        format!(
            "{} - - [{}] \"{} {} HTTP/1.1\" {} {} \"{}\" \"{}\" {}ms upstream={}",
            self.remote_addr,
            "-",
            self.method,
            self.uri,
            self.status,
            self.body_bytes,
            self.referer,
            self.user_agent,
            duration_ms,
            upstream,
        )
    }

    pub fn format_clf(&self) -> String {
        format!(
            "{} - - [{}] \"{} {} HTTP/1.1\" {} {}",
            self.remote_addr, "-", self.method, self.uri, self.status, self.body_bytes,
        )
    }

    pub fn format_json(&self) -> String {
        let duration_ms = self.duration.as_millis();
        let mut out = format!(
            "{{\"remote_addr\":\"{}\",\"method\":\"{}\",\"uri\":\"{}\",\"status\":{},\"body_bytes\":{},\"duration_ms\":{}}}",
            escape_json(&self.remote_addr),
            escape_json(&self.method.0),
            escape_json(&self.uri),
            self.status,
            self.body_bytes,
            duration_ms,
        );
        if let Some(ref addr) = self.upstream_addr {
            let _ = write!(out, ",\"upstream_addr\":\"{}\"", escape_json(addr));
        }
        if let Some(ref id) = self.request_id {
            let _ = write!(out, ",\"request_id\":\"{}\"", escape_json(id));
        }
        out.push('}');
        out
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Status code category for log coloring/classification.
pub const fn status_category(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> AccessLogEntry {
        AccessLogEntry {
            remote_addr: "127.0.0.1".into(),
            method: LogMethod("GET".into()),
            uri: "/index.html".into(),
            status: 200,
            body_bytes: 1234,
            duration: Duration::from_millis(42),
            referer: "https://example.com/".into(),
            user_agent: "test-agent".into(),
            upstream_addr: Some("10.0.0.1:8080".into()),
            request_id: Some("abc-123".into()),
        }
    }

    #[test]
    fn clf_format() {
        let entry = sample_entry();
        let line = entry.format_clf();
        assert!(line.contains("GET /index.html HTTP/1.1"));
        assert!(line.contains("200 1234"));
    }

    #[test]
    fn combined_format() {
        let entry = sample_entry();
        let line = entry.format_combined();
        assert!(line.contains("127.0.0.1"));
        assert!(line.contains("42ms"));
        assert!(line.contains("upstream=10.0.0.1:8080"));
    }

    #[test]
    fn json_format() {
        let entry = sample_entry();
        let json = entry.format_json();
        assert!(json.contains("\"status\":200"));
        assert!(json.contains("\"duration_ms\":42"));
        assert!(json.contains("\"request_id\":\"abc-123\""));
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
    }

    #[test]
    fn json_escapes_special_chars() {
        let mut entry = sample_entry();
        entry.uri = "/path?q=\"hello\"\\".into();
        let json = entry.format_json();
        assert!(json.contains("\\\"hello\\\""));
    }

    #[test]
    fn no_upstream_or_request_id() {
        let mut entry = sample_entry();
        entry.upstream_addr = None;
        entry.request_id = None;
        let line = entry.format_combined();
        assert!(line.contains("upstream=-"));

        let json = entry.format_json();
        assert!(!json.contains("upstream_addr"));
        assert!(!json.contains("request_id"));
    }

    #[test]
    fn status_categories() {
        assert_eq!(status_category(199), "1xx");
        assert_eq!(status_category(200), "2xx");
        assert_eq!(status_category(301), "3xx");
        assert_eq!(status_category(404), "4xx");
        assert_eq!(status_category(500), "5xx");
        assert_eq!(status_category(999), "other");
    }

    #[test]
    fn zero_body_bytes() {
        let mut entry = sample_entry();
        entry.body_bytes = 0;
        let line = entry.format_clf();
        assert!(line.contains("200 0"));
    }
}
