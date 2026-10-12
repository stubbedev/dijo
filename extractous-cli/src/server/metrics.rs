use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const DURATION_BUCKETS: [f64; 11] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0];

/// Request and extraction counters, rendered in the Prometheus text format.
#[derive(Default)]
pub struct Metrics {
    requests: Mutex<BTreeMap<(&'static str, u16), u64>>,
    failures: Mutex<BTreeMap<String, u64>>,
    durations: Mutex<BTreeMap<String, Histogram>>,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    pub in_flight: AtomicUsize,
    pub workers: AtomicUsize,
}

#[derive(Default)]
struct Histogram {
    buckets: [u64; DURATION_BUCKETS.len()],
    count: u64,
    sum: f64,
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

impl Metrics {
    pub fn request(&self, endpoint: &'static str, status: u16) {
        *self
            .requests
            .lock()
            .expect("metrics lock")
            .entry((endpoint, status))
            .or_default() += 1;
    }

    pub fn failure(&self, category: &str) {
        *self
            .failures
            .lock()
            .expect("metrics lock")
            .entry(category.to_string())
            .or_default() += 1;
    }

    pub fn extraction(&self, mime_type: &str, duration: Duration) {
        let seconds = duration.as_secs_f64();
        let mut durations = self.durations.lock().expect("metrics lock");
        let histogram = durations.entry(mime_type.to_string()).or_default();
        for (bucket, bound) in histogram.buckets.iter_mut().zip(DURATION_BUCKETS) {
            if seconds <= bound {
                *bucket += 1;
            }
        }
        histogram.count += 1;
        histogram.sum += seconds;
    }

    pub fn bytes_in(&self, bytes: u64) {
        self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn bytes_out(&self, bytes: u64) {
        self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# TYPE extractous_requests_total counter\n");
        for ((endpoint, status), count) in self.requests.lock().expect("metrics lock").iter() {
            let _ = writeln!(
                out,
                "extractous_requests_total{{endpoint=\"{endpoint}\",status=\"{status}\"}} {count}"
            );
        }
        out.push_str("# TYPE extractous_failures_total counter\n");
        for (category, count) in self.failures.lock().expect("metrics lock").iter() {
            let _ = writeln!(
                out,
                "extractous_failures_total{{category=\"{}\"}} {count}",
                escape_label(category)
            );
        }
        out.push_str("# TYPE extractous_extraction_seconds histogram\n");
        for (mime_type, histogram) in self.durations.lock().expect("metrics lock").iter() {
            let mime_type = escape_label(mime_type);
            for (bound, count) in DURATION_BUCKETS.iter().zip(histogram.buckets) {
                let _ = writeln!(
                    out,
                    "extractous_extraction_seconds_bucket{{mime_type=\"{mime_type}\",le=\"{bound}\"}} {count}"
                );
            }
            let _ = writeln!(
                out,
                "extractous_extraction_seconds_bucket{{mime_type=\"{mime_type}\",le=\"+Inf\"}} {}",
                histogram.count
            );
            let _ = writeln!(
                out,
                "extractous_extraction_seconds_sum{{mime_type=\"{mime_type}\"}} {}",
                histogram.sum
            );
            let _ = writeln!(
                out,
                "extractous_extraction_seconds_count{{mime_type=\"{mime_type}\"}} {}",
                histogram.count
            );
        }
        let totals = [
            (
                "extractous_bytes_in_total",
                "counter",
                self.bytes_in.load(Ordering::Relaxed),
            ),
            (
                "extractous_bytes_out_total",
                "counter",
                self.bytes_out.load(Ordering::Relaxed),
            ),
            (
                "extractous_in_flight_requests",
                "gauge",
                self.in_flight.load(Ordering::Relaxed) as u64,
            ),
            (
                "extractous_workers",
                "gauge",
                self.workers.load(Ordering::Relaxed) as u64,
            ),
        ];
        for (name, kind, value) in totals {
            let _ = writeln!(out, "# TYPE {name} {kind}\n{name} {value}");
        }
        out
    }
}
