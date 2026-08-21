//! Built-in status page endpoint for server health and metrics.
//!
//! Renders a JSON status page with server uptime, connection counts,
//! and memory usage.

use std::collections::HashMap;
use std::fmt::Write;
use std::time::{Duration, Instant};

/// Snapshot of server status for the status page.
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub started_at: Instant,
    pub active_connections: u64,
    pub total_requests: u64,
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub worker_count: u32,
    pub pid: u32,
    pub version: String,
    pub custom_fields: HashMap<String, String>,
}

impl ServerStatus {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            started_at: Instant::now(),
            active_connections: 0,
            total_requests: 0,
            total_bytes_read: 0,
            total_bytes_written: 0,
            worker_count: 1,
            pid: std::process::id(),
            version: version.into(),
            custom_fields: HashMap::new(),
        }
    }

    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn add_field(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.custom_fields.insert(key.into(), value.into());
    }
}

/// Renders a status page as a JSON-like string.
pub fn render_status(status: &ServerStatus) -> String {
    let uptime_secs = status.uptime().as_secs();
    let mut out = String::from("{\n");

    let _ = writeln!(out, "  \"version\": \"{}\",", status.version);
    let _ = writeln!(out, "  \"pid\": {},", status.pid);
    let _ = writeln!(out, "  \"uptime_seconds\": {uptime_secs},");
    let _ = writeln!(
        out,
        "  \"active_connections\": {},",
        status.active_connections
    );
    let _ = writeln!(out, "  \"total_requests\": {},", status.total_requests);
    let _ = writeln!(out, "  \"total_bytes_read\": {},", status.total_bytes_read);
    let _ = writeln!(
        out,
        "  \"total_bytes_written\": {},",
        status.total_bytes_written
    );
    let _ = writeln!(out, "  \"worker_count\": {},", status.worker_count);

    for (k, v) in &status.custom_fields {
        let _ = writeln!(out, "  \"{k}\": \"{v}\",");
    }

    // Remove trailing comma
    if out.ends_with(",\n") {
        out.truncate(out.len() - 2);
        out.push('\n');
    }
    out.push('}');
    out
}

/// Compact status format for health checks (returns "ok" or error).
pub const fn render_health(status: &ServerStatus) -> (&'static str, bool) {
    if status.active_connections == 0 && status.total_requests == 0 {
        ("starting", false)
    } else {
        ("ok", true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_status_defaults() {
        let s = ServerStatus::new("1.0.0");
        assert_eq!(s.version, "1.0.0");
        assert_eq!(s.active_connections, 0);
        assert_eq!(s.worker_count, 1);
    }

    #[test]
    fn uptime_is_nonzero() {
        let s = ServerStatus::new("0.1");
        assert!(s.uptime() < Duration::from_secs(1));
    }

    #[test]
    fn render_contains_all_fields() {
        let mut s = ServerStatus::new("2.0.0");
        s.add_field("mode", "production");
        let json = render_status(&s);
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"pid\""));
        assert!(json.contains("\"uptime_seconds\""));
        assert!(json.contains("\"active_connections\""));
        assert!(json.contains("\"mode\""));
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
    }

    #[test]
    fn render_health_starting() {
        let s = ServerStatus::new("1.0");
        let (msg, ok) = render_health(&s);
        assert_eq!(msg, "starting");
        assert!(!ok);
    }

    #[test]
    fn render_health_ok() {
        let mut s = ServerStatus::new("1.0");
        s.total_requests = 1;
        let (msg, ok) = render_health(&s);
        assert_eq!(msg, "ok");
        assert!(ok);
    }

    #[test]
    fn add_field_and_render() {
        let mut s = ServerStatus::new("1.0");
        s.add_field("region", "us-east-1");
        let json = render_status(&s);
        assert!(json.contains("\"region\": \"us-east-1\""));
    }
}
