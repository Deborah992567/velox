//! Prometheus text format exporter.
//!
//! Phase 21: Renders metrics in the Prometheus exposition format.

use std::fmt::Write as _;

use crate::metrics::registry::Registry;

/// Render all metrics in a `Registry` as Prometheus text.
pub fn render(registry: &Registry) -> String {
    let mut out = String::with_capacity(4096);

    if let Ok(counters) = registry.counters.read() {
        for c in counters.values() {
            let _ = writeln!(out, "# HELP {} counter", c.name());
            let _ = writeln!(out, "# TYPE {} counter", c.name());
            let _ = writeln!(out, "{} {}", c.name(), c.get());
        }
    }

    if let Ok(gauges) = registry.gauges.read() {
        for g in gauges.values() {
            let _ = writeln!(out, "# HELP {} gauge", g.name());
            let _ = writeln!(out, "# TYPE {} gauge", g.name());
            let _ = writeln!(out, "{} {}", g.name(), g.get());
        }
    }

    if let Ok(histograms) = registry.histograms.read() {
        for h in histograms.values() {
            let name = h.name();
            let _ = writeln!(out, "# HELP {name} histogram");
            let _ = writeln!(out, "# TYPE {name} histogram");
            let snap = h.snapshot();
            for &(le, count) in &snap.buckets {
                let le_label = if le.is_infinite() {
                    "+Inf".to_string()
                } else if le.fract() == 0.0 {
                    format!("{le:.0}")
                } else {
                    format!("{le}")
                };
                let _ = writeln!(out, "{name}_bucket{{le=\"{le_label}\"}} {count}");
            }
            let _ = writeln!(out, "{name}_sum {}", snap.sum);
            let _ = writeln!(out, "{name}_count {}", snap.count);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_registry() {
        let reg = Registry::new();
        let text = render(&reg);
        assert!(text.is_empty());
    }

    #[test]
    fn render_counter() {
        let reg = Registry::new();
        let c = reg.counter("http_requests_total");
        c.add(42);
        let text = render(&reg);
        assert!(text.contains("# TYPE http_requests_total counter"));
        assert!(text.contains("http_requests_total 42"));
    }

    #[test]
    fn render_gauge() {
        let reg = Registry::new();
        let g = reg.gauge("active_connections");
        g.set(7);
        let text = render(&reg);
        assert!(text.contains("# TYPE active_connections gauge"));
        assert!(text.contains("active_connections 7"));
    }

    #[test]
    fn render_histogram() {
        let reg = Registry::new();
        let h = reg.histogram("request_duration_seconds", &[0.1, 1.0]);
        h.record(0.05);
        h.record(0.5);
        let text = render(&reg);
        assert!(text.contains("# TYPE request_duration_seconds histogram"));
        assert!(text.contains("request_duration_seconds_count 2"));
        assert!(text.contains("request_duration_seconds_bucket{le=\"0.1\"} 1"));
        assert!(text.contains("request_duration_seconds_bucket{le=\"1\"} 2"));
        assert!(text.contains("request_duration_seconds_bucket{le=\"+Inf\"} 2"));
    }
}
