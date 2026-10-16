use config::configuration;
use config::lua::get_or_create_sub_module;
use config::lua::mlua::Lua;
use hdrhistogram::Histogram;
use metrics::{Counter, Gauge, Key, KeyName, Metadata, Recorder, SharedString, Unit};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tabout::{tabulate_output, Alignment, Column};

static ENABLE_STAT_PRINT: AtomicBool = AtomicBool::new(true);
lazy_static::lazy_static! {
    static ref INNER: Arc<Mutex<Inner>> = make_inner();
}

struct ThroughputInner {
    hist: Histogram<u64>,
    last: Option<Instant>,
    count: u64,
}

struct Throughput {
    inner: Mutex<ThroughputInner>,
}

impl Throughput {
    fn new() -> Self {
        Self {
            inner: Mutex::new(ThroughputInner {
                hist: Histogram::new(2).expect("failed to create histogram"),
                last: None,
                count: 0,
            }),
        }
    }
    fn current(&self) -> u64 {
        self.inner.lock().current()
    }

    fn percentiles(&self) -> (u64, u64, u64) {
        let inner = self.inner.lock();
        let p50 = inner.hist.value_at_percentile(50.);
        let p75 = inner.hist.value_at_percentile(75.);
        let p95 = inner.hist.value_at_percentile(95.);
        (p50, p75, p95)
    }
}

impl ThroughputInner {
    fn add(&mut self, value: u64) {
        if let Some(ref last) = self.last {
            let elapsed = last.elapsed();
            if elapsed > Duration::from_secs(1) {
                self.hist.record(self.count).ok();
                self.count = 0;
                self.last = Some(Instant::now());
            }
        } else {
            // Start a new window
            self.last = Some(Instant::now());
        };
        self.count += value;
    }

    fn current(&mut self) -> u64 {
        if let Some(ref last) = self.last {
            let elapsed = last.elapsed();
            if elapsed > Duration::from_secs(1) {
                self.hist.record(self.count).ok();
                self.count = 0;
                self.last = Some(Instant::now());
            }
        }
        self.count
    }
}

impl metrics::HistogramFn for Throughput {
    fn record(&self, value: f64) {
        self.inner.lock().add(value as u64);
    }
}

struct ScaledHistogram {
    hist: Mutex<Histogram<u64>>,
    scale: f64,
}

/// The summary of a value histogram: the mean, selected percentiles
/// and the peak (max) value observed so far.
struct Snapshot {
    mean: f64,
    p50: u64,
    p75: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

impl ScaledHistogram {
    fn new(scale: f64) -> Arc<Self> {
        Arc::new(Self {
            hist: Mutex::new(Histogram::new(2).expect("failed to create new Histogram")),
            scale,
        })
    }
    fn snapshot(&self) -> Snapshot {
        let hist = self.hist.lock();
        Snapshot {
            mean: hist.mean(),
            p50: hist.value_at_percentile(50.),
            p75: hist.value_at_percentile(75.),
            p95: hist.value_at_percentile(95.),
            p99: hist.value_at_percentile(99.),
            max: hist.max(),
        }
    }
}

impl metrics::HistogramFn for ScaledHistogram {
    fn record(&self, value: f64) {
        self.hist.lock().record((value * self.scale) as u64).ok();
    }
}

struct MyCounter {
    value: AtomicUsize,
}

impl metrics::CounterFn for MyCounter {
    fn increment(&self, value: u64) {
        self.value.fetch_add(value as usize, Ordering::Relaxed);
    }

    fn absolute(&self, value: u64) {
        self.value.store(value as usize, Ordering::Relaxed);
    }
}

struct Inner {
    histograms: HashMap<Key, Arc<ScaledHistogram>>,
    throughput: HashMap<Key, Arc<Throughput>>,
    counters: HashMap<Key, Arc<MyCounter>>,
}

impl Inner {
    fn run(inner: Arc<Mutex<Inner>>) {
        let mut last_print = Instant::now();

        let rate_cols = vec![
            Column {
                name: "STAT".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "current".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p50".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p75".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p95".to_string(),
                alignment: Alignment::Left,
            },
        ];
        let cols = vec![
            Column {
                name: "STAT".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "mean".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p50".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p75".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p95".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "p99".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "max".to_string(),
                alignment: Alignment::Left,
            },
        ];
        let count_cols = vec![
            Column {
                name: "STAT".to_string(),
                alignment: Alignment::Left,
            },
            Column {
                name: "COUNT".to_string(),
                alignment: Alignment::Left,
            },
        ];

        loop {
            std::thread::sleep(Duration::from_secs(1));

            if !ENABLE_STAT_PRINT.load(Ordering::Acquire) {
                break;
            }

            let seconds = configuration().periodic_stat_logging;
            if seconds == 0 {
                continue;
            }
            if last_print.elapsed() >= Duration::from_secs(seconds) {
                let mut data = vec![];

                let mut inner = inner.lock();
                for (key, tput) in &mut inner.throughput {
                    let current = tput.current();
                    let (p50, p75, p95) = tput.percentiles();
                    data.push(vec![
                        key.to_string(),
                        format!("{:.2?}", current),
                        format!("{:.2?}", p50),
                        format!("{:.2?}", p75),
                        format!("{:.2?}", p95),
                    ]);
                }
                data.sort_by(|a, b| a[0].cmp(&b[0]));
                eprintln!();
                tabulate_output(&rate_cols, &data, &mut std::io::stderr().lock()).ok();

                data.clear();
                for (key, histogram) in &inner.histograms {
                    let snap = histogram.snapshot();
                    if key.name().ends_with(".size") {
                        data.push(vec![
                            key.to_string(),
                            format!("{:.2}", snap.mean),
                            format!("{:.2?}", snap.p50),
                            format!("{:.2?}", snap.p75),
                            format!("{:.2?}", snap.p95),
                            format!("{:.2?}", snap.p99),
                            format!("{:.2?}", snap.max),
                        ]);
                    } else {
                        data.push(vec![
                            key.to_string(),
                            format!(
                                "{:.2?}",
                                Duration::from_secs_f64(snap.mean / 1_000_000_000.)
                            ),
                            format!("{:.2?}", Duration::from_nanos(snap.p50)),
                            format!("{:.2?}", Duration::from_nanos(snap.p75)),
                            format!("{:.2?}", Duration::from_nanos(snap.p95)),
                            format!("{:.2?}", Duration::from_nanos(snap.p99)),
                            format!("{:.2?}", Duration::from_nanos(snap.max)),
                        ]);
                    }
                }
                data.sort_by(|a, b| a[0].cmp(&b[0]));
                eprintln!();
                tabulate_output(&cols, &data, &mut std::io::stderr().lock()).ok();

                data.clear();
                for (key, count) in &inner.counters {
                    data.push(vec![
                        key.to_string(),
                        count.value.load(Ordering::Relaxed).to_string(),
                    ]);
                }
                data.sort_by(|a, b| a[0].cmp(&b[0]));
                eprintln!();
                tabulate_output(&count_cols, &data, &mut std::io::stderr().lock()).ok();

                last_print = Instant::now();
            }
        }
    }
}

fn make_inner() -> Arc<Mutex<Inner>> {
    Arc::new(Mutex::new(Inner {
        histograms: HashMap::new(),
        throughput: HashMap::new(),
        counters: HashMap::new(),
    }))
}

pub struct Stats {
    inner: Arc<Mutex<Inner>>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            inner: Arc::clone(&INNER),
        }
    }

    pub fn init() -> anyhow::Result<()> {
        let stats = Self::new();
        let inner = Arc::clone(&stats.inner);
        std::thread::spawn(move || Inner::run(inner));
        metrics::set_global_recorder(stats)
            .map_err(|e| anyhow::anyhow!("Failed to set metrics recorder:{}", e))
    }
}

impl Recorder for Stats {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata) -> Counter {
        let mut inner = self.inner.lock();
        match inner.counters.get(key) {
            Some(existing) => Counter::from_arc(existing.clone()),
            None => {
                let counter = Arc::new(MyCounter {
                    value: AtomicUsize::new(0),
                });
                inner.counters.insert(key.clone(), counter.clone());
                metrics::Counter::from_arc(counter)
            }
        }
    }

    fn register_gauge(&self, _key: &Key, _metadata: &Metadata) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, key: &Key, _metadata: &Metadata) -> metrics::Histogram {
        let mut inner = self.inner.lock();
        if key.name().ends_with(".rate") {
            match inner.throughput.get(key) {
                Some(existing) => metrics::Histogram::from_arc(existing.clone()),
                None => {
                    let tput = Arc::new(Throughput::new());
                    inner.throughput.insert(key.clone(), tput.clone());

                    metrics::Histogram::from_arc(tput)
                }
            }
        } else {
            match inner.histograms.get(key) {
                Some(existing) => metrics::Histogram::from_arc(existing.clone()),
                None => {
                    let scale = if key.name().ends_with(".size") {
                        1.0
                    } else {
                        // Assume seconds; convert to nanoseconds
                        1_000_000_000.0
                    };

                    let histogram = ScaledHistogram::new(scale);
                    inner.histograms.insert(key.clone(), histogram.clone());

                    metrics::Histogram::from_arc(histogram)
                }
            }
        }
    }
}

pub fn register(lua: &Lua) -> anyhow::Result<()> {
    let metrics_mod = get_or_create_sub_module(lua, "metrics")?;
    metrics_mod.set(
        "get_counters",
        lua.create_function(|_, _: ()| {
            let inner = INNER.lock();
            let counters: HashMap<String, usize> = inner
                .counters
                .iter()
                .map(|(k, v)| (k.name().to_string(), v.value.load(Ordering::Relaxed)))
                .collect();
            Ok(counters)
        })?,
    )?;
    metrics_mod.set(
        "get_throughput",
        lua.create_function(|_, _: ()| {
            let mut inner = INNER.lock();
            let counters: HashMap<String, HashMap<String, u64>> = inner
                .throughput
                .iter_mut()
                .map(|(k, tput)| {
                    let mut res = HashMap::new();
                    res.insert("current".to_string(), tput.current());
                    let (p50, p75, p95) = tput.percentiles();
                    res.insert("p50".to_string(), p50);
                    res.insert("p75".to_string(), p75);
                    res.insert("p95".to_string(), p95);
                    (k.name().to_string(), res)
                })
                .collect();
            Ok(counters)
        })?,
    )?;
    metrics_mod.set(
        "get_sizes",
        lua.create_function(|_, _: ()| {
            let inner = INNER.lock();
            let counters: HashMap<String, HashMap<String, u64>> = inner
                .histograms
                .iter()
                .filter_map(|(key, hist)| {
                    if key.name().ends_with(".size") {
                        let snap = hist.snapshot();
                        let mut res = HashMap::new();
                        res.insert("mean".to_string(), snap.mean as u64);
                        res.insert("p50".to_string(), snap.p50);
                        res.insert("p75".to_string(), snap.p75);
                        res.insert("p95".to_string(), snap.p95);
                        res.insert("p99".to_string(), snap.p99);
                        res.insert("max".to_string(), snap.max);
                        Some((key.name().to_string(), res))
                    } else {
                        None
                    }
                })
                .collect();
            Ok(counters)
        })?,
    )?;
    metrics_mod.set(
        "get_latency",
        lua.create_function(|_, _: ()| {
            let inner = INNER.lock();
            let counters: HashMap<String, HashMap<String, String>> = inner
                .histograms
                .iter()
                .filter_map(|(key, hist)| {
                    if !key.name().ends_with(".size") {
                        let snap = hist.snapshot();
                        let mut res = HashMap::new();
                        res.insert(
                            "mean".to_string(),
                            format!("{:?}", Duration::from_secs_f64(snap.mean / 1_000_000_000.)),
                        );
                        res.insert(
                            "p50".to_string(),
                            format!("{:?}", Duration::from_nanos(snap.p50)),
                        );
                        res.insert(
                            "p75".to_string(),
                            format!("{:?}", Duration::from_nanos(snap.p75)),
                        );
                        res.insert(
                            "p95".to_string(),
                            format!("{:?}", Duration::from_nanos(snap.p95)),
                        );
                        res.insert(
                            "p99".to_string(),
                            format!("{:?}", Duration::from_nanos(snap.p99)),
                        );
                        res.insert(
                            "max".to_string(),
                            format!("{:?}", Duration::from_nanos(snap.max)),
                        );
                        Some((key.name().to_string(), res))
                    } else {
                        None
                    }
                })
                .collect();
            Ok(counters)
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ScaledHistogram::snapshot reports the mean, percentiles and the
    /// peak value of everything recorded so far
    #[test]
    fn snapshot_reports_mean_percentiles_and_max() {
        let hist = ScaledHistogram::new(1.0);
        // Record the values 1..=100; with 2 significant digits the
        // histogram is precise enough for the assertions below
        for value in 1..=100u64 {
            metrics::HistogramFn::record(&*hist, value as f64);
        }
        let snap = hist.snapshot();
        assert!(
            (45.0..=56.0).contains(&snap.mean),
            "mean should be around 50.5, got {}",
            snap.mean
        );
        assert_eq!(snap.max, 100, "peak value");
        assert!(
            snap.p50 >= 45 && snap.p50 <= 56,
            "p50 should be around 50, got {}",
            snap.p50
        );
        assert!(
            snap.p95 >= 90 && snap.p95 <= 100,
            "p95 should be around 95, got {}",
            snap.p95
        );
        assert!(
            snap.p99 >= 95 && snap.p99 <= 100,
            "p99 should be around 99, got {}",
            snap.p99
        );
        // Percentiles are ordered
        assert!(snap.p50 <= snap.p75);
        assert!(snap.p75 <= snap.p95);
        assert!(snap.p95 <= snap.p99);
        assert!(snap.p99 <= snap.max);
    }
}
