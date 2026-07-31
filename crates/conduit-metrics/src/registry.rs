//! A small Prometheus registry.
//!
//! Hand-written rather than pulled in: Conduit needs three metric types and
//! one text format, the format is a published spec, and the exposition is
//! easier to test directly than through someone else's abstraction.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

/// Label values for one time series, kept sorted so output is stable.
pub type Labels = Vec<(&'static str, String)>;

/// Builds a label set.
#[must_use]
pub fn labels(pairs: &[(&'static str, &str)]) -> Labels {
    let mut labels: Labels =
        pairs.iter().map(|(name, value)| (*name, (*value).to_owned())).collect();
    labels.sort_by(|left, right| left.0.cmp(right.0));
    labels
}

/// Anything the registry can render.
trait Render: Send + Sync {
    fn render(&self, out: &mut String);
}

/// A collection of metrics, rendered together.
#[derive(Default)]
pub struct Registry {
    metrics: Mutex<Vec<Arc<dyn Render>>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a counter.
    pub fn counter(&self, name: &'static str, help: &'static str) -> Arc<Counter> {
        let counter = Arc::new(Counter { name, help, series: Mutex::new(BTreeMap::new()) });
        self.add(Arc::clone(&counter) as Arc<dyn Render>);
        counter
    }

    /// Registers a gauge.
    pub fn gauge(&self, name: &'static str, help: &'static str) -> Arc<Gauge> {
        let gauge = Arc::new(Gauge { name, help, series: Mutex::new(BTreeMap::new()) });
        self.add(Arc::clone(&gauge) as Arc<dyn Render>);
        gauge
    }

    /// Registers a histogram with the given upper bounds, in ascending order.
    pub fn histogram(
        &self,
        name: &'static str,
        help: &'static str,
        bounds: &[f64],
    ) -> Arc<Histogram> {
        let histogram = Arc::new(Histogram {
            name,
            help,
            bounds: bounds.to_vec(),
            series: Mutex::new(BTreeMap::new()),
        });
        self.add(Arc::clone(&histogram) as Arc<dyn Render>);
        histogram
    }

    /// Renders every metric in the Prometheus text exposition format.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for metric in self.metrics.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            metric.render(&mut out);
        }
        out
    }

    fn add(&self, metric: Arc<dyn Render>) {
        self.metrics.lock().unwrap_or_else(PoisonError::into_inner).push(metric);
    }
}

/// A monotonically increasing count.
#[derive(Debug)]
pub struct Counter {
    name: &'static str,
    help: &'static str,
    series: Mutex<BTreeMap<Labels, u64>>,
}

impl Counter {
    /// Adds one to the series identified by `labels`.
    pub fn increment(&self, labels: Labels) {
        self.add(labels, 1);
    }

    /// Adds `amount` to the series identified by `labels`.
    pub fn add(&self, labels: Labels, amount: u64) {
        let mut series = self.series.lock().unwrap_or_else(PoisonError::into_inner);
        *series.entry(labels).or_insert(0) += amount;
    }

    /// The current value of a series, for tests.
    #[must_use]
    pub fn value(&self, labels: &Labels) -> u64 {
        self.series
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(labels)
            .copied()
            .unwrap_or(0)
    }
}

impl Render for Counter {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "counter");
        for (labels, value) in self.series.lock().unwrap_or_else(PoisonError::into_inner).iter()
        {
            let _ = writeln!(out, "{}{} {value}", self.name, format_labels(labels, None));
        }
    }
}

/// A value that goes up and down.
#[derive(Debug)]
pub struct Gauge {
    name: &'static str,
    help: &'static str,
    series: Mutex<BTreeMap<Labels, i64>>,
}

impl Gauge {
    /// Adds one.
    pub fn increment(&self, labels: Labels) {
        self.add(labels, 1);
    }

    /// Subtracts one, never going below zero.
    ///
    /// A negative count of active conversations would be a lie about the
    /// system rather than a useful signal, so the floor is deliberate.
    pub fn decrement(&self, labels: Labels) {
        self.add(labels, -1);
    }

    /// Adds `amount`, clamped at zero.
    pub fn add(&self, labels: Labels, amount: i64) {
        let mut series = self.series.lock().unwrap_or_else(PoisonError::into_inner);
        let value = series.entry(labels).or_insert(0);
        *value = (*value + amount).max(0);
    }

    /// The current value of a series, for tests.
    #[must_use]
    pub fn value(&self, labels: &Labels) -> i64 {
        self.series
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(labels)
            .copied()
            .unwrap_or(0)
    }
}

impl Render for Gauge {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "gauge");
        for (labels, value) in self.series.lock().unwrap_or_else(PoisonError::into_inner).iter()
        {
            let _ = writeln!(out, "{}{} {value}", self.name, format_labels(labels, None));
        }
    }
}

/// Bucket counts and running totals for one series.
#[derive(Debug, Clone)]
struct Buckets {
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

/// A distribution of observations across fixed buckets.
#[derive(Debug)]
pub struct Histogram {
    name: &'static str,
    help: &'static str,
    bounds: Vec<f64>,
    series: Mutex<BTreeMap<Labels, Buckets>>,
}

impl Histogram {
    /// Records `value` against the series identified by `labels`.
    pub fn observe(&self, labels: Labels, value: f64) {
        let mut series = self.series.lock().unwrap_or_else(PoisonError::into_inner);
        let buckets = series.entry(labels).or_insert_with(|| Buckets {
            counts: vec![0; self.bounds.len()],
            sum: 0.0,
            count: 0,
        });

        // Prometheus buckets are cumulative: an observation lands in its own
        // bucket and every wider one.
        for (index, bound) in self.bounds.iter().enumerate() {
            if value <= *bound {
                buckets.counts[index] += 1;
            }
        }
        buckets.sum += value;
        buckets.count += 1;
    }

    /// How many observations a series has, for tests.
    #[must_use]
    pub fn count(&self, labels: &Labels) -> u64 {
        self.series
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(labels)
            .map_or(0, |buckets| buckets.count)
    }
}

impl Render for Histogram {
    fn render(&self, out: &mut String) {
        header(out, self.name, self.help, "histogram");
        for (labels, buckets) in
            self.series.lock().unwrap_or_else(PoisonError::into_inner).iter()
        {
            for (index, bound) in self.bounds.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{}_bucket{} {}",
                    self.name,
                    format_labels(labels, Some(&format_float(*bound))),
                    buckets.counts[index]
                );
            }
            let _ = writeln!(
                out,
                "{}_bucket{} {}",
                self.name,
                format_labels(labels, Some("+Inf")),
                buckets.count
            );
            let _ = writeln!(
                out,
                "{}_sum{} {}",
                self.name,
                format_labels(labels, None),
                format_float(buckets.sum)
            );
            let _ = writeln!(
                out,
                "{}_count{} {}",
                self.name,
                format_labels(labels, None),
                buckets.count
            );
        }
    }
}

/// Writes the `HELP` and `TYPE` lines of a metric family.
fn header(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// Renders a label set, optionally with a histogram bucket bound.
fn format_labels(labels: &Labels, le: Option<&str>) -> String {
    if labels.is_empty() && le.is_none() {
        return String::new();
    }

    let mut parts: Vec<String> =
        labels.iter().map(|(name, value)| format!("{name}=\"{}\"", escape(value))).collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    format!("{{{}}}", parts.join(","))
}

/// Escapes a label value per the exposition format.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Formats a float without a trailing `.0`, which Prometheus dislikes on `le`.
fn format_float(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counter_renders_with_its_help_and_type() {
        let registry = Registry::new();
        let counter = registry.counter("conduit_events_total", "Events published.");
        counter.increment(labels(&[("stage", "reasoning")]));
        counter.increment(labels(&[("stage", "reasoning")]));

        let output = registry.render();
        assert!(output.contains("# HELP conduit_events_total Events published."));
        assert!(output.contains("# TYPE conduit_events_total counter"));
        assert!(output.contains("conduit_events_total{stage=\"reasoning\"} 2"));
    }

    #[test]
    fn an_unlabelled_series_has_no_braces() {
        let registry = Registry::new();
        registry.counter("conduit_turns_total", "Turns.").increment(Vec::new());
        assert!(registry.render().contains("\nconduit_turns_total 1\n"));
    }

    #[test]
    fn labels_render_in_a_stable_order() {
        // Unstable ordering makes diffs and dashboards noisy.
        let registry = Registry::new();
        let counter = registry.counter("m", "help");
        counter.increment(labels(&[("zebra", "1"), ("alpha", "2")]));
        assert!(registry.render().contains("m{alpha=\"2\",zebra=\"1\"} 1"));
    }

    #[test]
    fn label_values_are_escaped() {
        let registry = Registry::new();
        registry.counter("m", "help").increment(labels(&[("error", "say \"hi\"\nnow")]));
        assert!(registry.render().contains(r#"m{error="say \"hi\"\nnow"} 1"#));
    }

    #[test]
    fn a_gauge_goes_up_and_down() {
        let registry = Registry::new();
        let gauge = registry.gauge("conduit_active", "Active conversations.");
        gauge.increment(Vec::new());
        gauge.increment(Vec::new());
        gauge.decrement(Vec::new());
        assert_eq!(gauge.value(&Vec::new()), 1);
        assert!(registry.render().contains("# TYPE conduit_active gauge"));
    }

    #[test]
    fn a_gauge_never_goes_negative() {
        let registry = Registry::new();
        let gauge = registry.gauge("conduit_active", "Active conversations.");
        gauge.decrement(Vec::new());
        assert_eq!(gauge.value(&Vec::new()), 0);
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let registry = Registry::new();
        let histogram = registry.histogram("conduit_latency", "Latency.", &[0.1, 0.5, 1.0]);
        histogram.observe(Vec::new(), 0.05);
        histogram.observe(Vec::new(), 0.4);
        histogram.observe(Vec::new(), 5.0);

        let output = registry.render();
        assert!(output.contains("conduit_latency_bucket{le=\"0.1\"} 1"), "{output}");
        assert!(output.contains("conduit_latency_bucket{le=\"0.5\"} 2"), "{output}");
        assert!(output.contains("conduit_latency_bucket{le=\"1.0\"} 2"), "{output}");
        assert!(output.contains("conduit_latency_bucket{le=\"+Inf\"} 3"), "{output}");
        assert!(output.contains("conduit_latency_count 3"), "{output}");
        assert!(output.contains("conduit_latency_sum 5.45"), "{output}");
    }

    #[test]
    fn a_histogram_keeps_its_series_apart() {
        let registry = Registry::new();
        let histogram = registry.histogram("m", "help", &[1.0]);
        histogram.observe(labels(&[("node", "stt")]), 0.5);
        assert_eq!(histogram.count(&labels(&[("node", "stt")])), 1);
        assert_eq!(histogram.count(&labels(&[("node", "tts")])), 0);
    }

    #[test]
    fn every_line_is_terminated() {
        // A body whose last line lacks a newline is rejected by some scrapers.
        let registry = Registry::new();
        registry.counter("m", "help").increment(Vec::new());
        assert!(registry.render().ends_with('\n'));
    }
}
