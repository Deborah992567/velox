//! Central metric registry.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::counter::Counter;
use super::gauge::Gauge;
use super::histogram::Histogram;

/// A metric registry that holds all registered metrics.
#[derive(Debug)]
pub struct Registry {
    pub(crate) counters: RwLock<HashMap<String, Arc<Counter>>>,
    pub(crate) gauges: RwLock<HashMap<String, Arc<Gauge>>>,
    pub(crate) histograms: RwLock<HashMap<String, Arc<Histogram>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        }
    }

    pub fn counter(&self, name: &str) -> Arc<Counter> {
        if let Ok(counters) = self.counters.read()
            && let Some(c) = counters.get(name)
        {
            return Arc::clone(c);
        }
        let c = Arc::new(Counter::new(name));
        if let Ok(mut counters) = self.counters.write() {
            counters
                .entry(name.to_string())
                .or_insert_with(|| Arc::clone(&c));
        }
        c
    }

    pub fn gauge(&self, name: &str) -> Arc<Gauge> {
        if let Ok(gauges) = self.gauges.read()
            && let Some(g) = gauges.get(name)
        {
            return Arc::clone(g);
        }
        let g = Arc::new(Gauge::new(name));
        if let Ok(mut gauges) = self.gauges.write() {
            gauges
                .entry(name.to_string())
                .or_insert_with(|| Arc::clone(&g));
        }
        g
    }

    pub fn histogram(&self, name: &str, boundaries: &[f64]) -> Arc<Histogram> {
        if let Ok(histograms) = self.histograms.read()
            && let Some(h) = histograms.get(name)
        {
            return Arc::clone(h);
        }
        let h = Arc::new(Histogram::new(name, boundaries));
        if let Ok(mut histograms) = self.histograms.write() {
            histograms
                .entry(name.to_string())
                .or_insert_with(|| Arc::clone(&h));
        }
        h
    }

    pub fn counter_count(&self) -> usize {
        self.counters.read().map_or(0, |m| m.len())
    }

    pub fn gauge_count(&self) -> usize {
        self.gauges.read().map_or(0, |m| m.len())
    }

    pub fn histogram_count(&self) -> usize {
        self.histograms.read().map_or(0, |m| m.len())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_reuse() {
        let reg = Registry::new();
        let c1 = reg.counter("requests");
        let c2 = reg.counter("requests");
        c1.inc();
        assert_eq!(c2.get(), 1);
    }

    #[test]
    fn gauge_reuse() {
        let reg = Registry::new();
        let g1 = reg.gauge("connections");
        let g2 = reg.gauge("connections");
        g1.inc();
        assert_eq!(g2.get(), 1);
    }

    #[test]
    fn histogram_reuse() {
        let reg = Registry::new();
        let h1 = reg.histogram("latency", &[0.1, 1.0]);
        let h2 = reg.histogram("latency", &[0.1, 1.0]);
        h1.record(0.05);
        assert_eq!(h2.count(), 1);
    }

    #[test]
    fn counts() {
        let reg = Registry::new();
        assert_eq!(reg.counter_count(), 0);
        reg.counter("a");
        reg.counter("b");
        assert_eq!(reg.counter_count(), 2);
        reg.gauge("g1");
        assert_eq!(reg.gauge_count(), 1);
        reg.histogram("h1", &[]);
        assert_eq!(reg.histogram_count(), 1);
    }

    #[test]
    fn different_names_are_distinct() {
        let reg = Registry::new();
        let c1 = reg.counter("a");
        let c2 = reg.counter("b");
        c1.inc();
        assert_eq!(c1.get(), 1);
        assert_eq!(c2.get(), 0);
    }
}
