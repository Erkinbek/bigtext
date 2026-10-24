//! Background-job plumbing: cancellation tokens, progress counters and UI wakers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cooperative cancellation flag shared between the UI and a worker.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Progress counter (`done` out of `total` units, usually bytes).
#[derive(Clone, Default, Debug)]
pub struct Progress {
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl Progress {
    pub fn new(total: u64) -> Self {
        let p = Self::default();
        p.set_total(total);
        p
    }
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }
    pub fn set_done(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
    pub fn fraction(&self) -> f32 {
        let t = self.total();
        if t == 0 {
            0.0
        } else {
            (self.done() as f64 / t as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Wakes the UI (normally `egui::Context::request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub fn noop_waker() -> Waker {
    Arc::new(|| {})
}

/// Rate limiter for wakeups/progress messages from tight worker loops.
pub struct Throttle {
    last: Instant,
    interval: Duration,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self { last: Instant::now(), interval }
    }
    /// Returns true at most once per interval.
    pub fn ready(&mut self) -> bool {
        if self.last.elapsed() >= self.interval {
            self.last = Instant::now();
            true
        } else {
            false
        }
    }
}

/// Spawns a named worker thread.
pub fn spawn<F: FnOnce() + Send + 'static>(name: &str, f: F) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .expect("failed to spawn worker thread")
}

/// Formats a byte count for humans ("1.23 GB").
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}
