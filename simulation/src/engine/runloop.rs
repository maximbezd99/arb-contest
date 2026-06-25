use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_affinity::CoreId;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use tracing::info;

use crate::cores;
use crate::net::tcp::ContestantSubmission;
use crate::protocol::feed::{FeedTick, PriceUpdate};

const SLEEP_SLACK_US: u64 = 50;
const LATE_THRESHOLD_NS: u64 = 10_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct StageStats {
    pub name: &'static str,
    pub unit: &'static str,
    pub count: u64,
    pub total: u64,
    pub avg: u64,
    pub p50: u64,
    pub p95: u64,
    pub max: u64,
}

impl StageStats {
    pub fn info(&self) {
        info!(
            stage = self.name,
            unit = self.unit,
            count = self.count,
            total = self.total,
            avg = self.avg,
            p50 = self.p50,
            p95 = self.p95,
            max = self.max,
            "runloop stage stats:",
        );
    }
}

#[derive(Debug, Clone)]
pub struct RunloopOutcome {
    /// Number of ticks dispatched to the UDP send thread.
    pub dispatched: usize,
    /// Errors sending to UDP.
    pub udp_dispatch_errors: u64,
    /// Total submission received from contestants.
    pub submissions_received: u64,
    /// Overshoot of the *last* tick. Small ⇒ we caught up by the end;
    /// If small - means simulation doesn't accumulate overshoot.
    pub final_overshoot_ns: u64,
    /// Count of ticks whose overshoot exceeded `LATE_THRESHOLD_NS`.
    /// Threshold-based count — separate concept from any percentile.
    pub late_ticks: u64,
    /// Read pass timing (submission drain + check) per iteration.
    pub read_stats: StageStats,
    /// Sleep + spin wait stage per iteration that didn't dispatch.
    pub wait_stats: StageStats,
    /// UDP-channel dispatch (`udp_tx.send`) per dispatch iteration.
    pub send_stats: StageStats,
    /// Whole-iteration timing.
    pub iter_stats: StageStats,
    /// Per-tick overshoot distribution.
    pub overshoot_stats: StageStats,
    /// Depth of the UDP send channel.
    pub udp_queue_depth_stats: StageStats,
}

pub fn spawn(
    core_id: CoreId,
    feed: Vec<FeedTick>,
    send_tx: Sender<PriceUpdate>,
    sub_rx: Receiver<ContestantSubmission>,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<RunloopOutcome> {
    thread::Builder::new()
        .name("engine-runloop".into())
        .spawn(move || {
            cores::pin_and_verify(core_id);
            run(feed, send_tx, sub_rx, shutdown)
        })
        .expect("spawn engine-runloop thread")
}

fn run(
    feed: Vec<FeedTick>,
    udp_tx: Sender<PriceUpdate>,
    sub_rx: Receiver<ContestantSubmission>,
    shutdown: Arc<AtomicBool>,
) -> RunloopOutcome {
    let mut overshoot_ns = 0u64;
    let mut late_ticks = 0u64;
    let mut dispatched = 0usize;
    let mut udp_dispatch_errors = 0u64;
    let mut submissions_received = 0u64;

    let feed_len = feed.len();
    let mut idx = 0usize;
    let mut target = Instant::now();
    if feed_len > 0 {
        target += Duration::from_micros(feed[0].delay_us);
    }

    let mut read_hist = Histogram::new();
    let mut wait_hist = Histogram::new();
    let mut send_hist = Histogram::new();
    let mut iter_hist = Histogram::new();
    let mut overshoot_hist = Histogram::new();
    let mut udp_queue_depth_hist = Histogram::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let iter_start = Instant::now();

        // 1. Read submissions.
        let read_start = iter_start;
        while let Ok(_) = sub_rx.try_recv() {
            submissions_received += 1;
            // submission handling logic
        }
        let after_read = Instant::now();
        read_hist.record((after_read - read_start).as_nanos() as u64);

        if idx >= feed_len {
            iter_hist.record((after_read - iter_start).as_nanos() as u64);
            break;
        }

        // 2. Check if can send tick.
        if after_read < target {
            let wait_start = after_read;
            if target > wait_start + Duration::from_micros(SLEEP_SLACK_US) {
                thread::sleep(target - wait_start - Duration::from_micros(SLEEP_SLACK_US));
            } else {
                std::hint::spin_loop();
            }
            let after_wait = Instant::now();
            wait_hist.record((after_wait - wait_start).as_nanos() as u64);
            iter_hist.record((after_wait - iter_start).as_nanos() as u64);
            continue;
        }

        // 3. Dispatch the tick to the UDP send thread.
        overshoot_ns = after_read.saturating_duration_since(target).as_nanos() as u64;
        if overshoot_ns > LATE_THRESHOLD_NS {
            late_ticks += 1;
        }
        overshoot_hist.record(overshoot_ns);

        let send_start = after_read;
        match udp_tx.try_send(feed[idx].update) {
            Ok(()) => {
                dispatched += 1;
                udp_queue_depth_hist.record(udp_tx.len() as u64);
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                udp_dispatch_errors += 1;
            }
        }
        let after_send = Instant::now();
        send_hist.record((after_send - send_start).as_nanos() as u64);
        iter_hist.record((after_send - iter_start).as_nanos() as u64);

        idx += 1;
        if idx < feed_len {
            target += Duration::from_micros(feed[idx].delay_us);
        }
    }

    RunloopOutcome {
        dispatched,
        udp_dispatch_errors,
        submissions_received,
        final_overshoot_ns: overshoot_ns,
        late_ticks,
        read_stats: read_hist.stats("read", "ns"),
        wait_stats: wait_hist.stats("wait", "ns"),
        send_stats: send_hist.stats("send", "ns"),
        iter_stats: iter_hist.stats("iter", "ns"),
        overshoot_stats: overshoot_hist.stats("overshoot", "ns"),
        udp_queue_depth_stats: udp_queue_depth_hist.stats("udp_queue_depth", "items"),
    }
}

struct Histogram {
    buckets: [u64; 64],
    total_ns: u128,
    count: u64,
    max_ns: u64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: [0; 64],
            total_ns: 0,
            count: 0,
            max_ns: 0,
        }
    }

    #[inline(always)]
    fn record(&mut self, value_ns: u64) {
        // bucket = floor(log2(value)); 0 → bucket 0.
        let bucket = if value_ns == 0 {
            0
        } else {
            (63 - value_ns.leading_zeros()) as usize
        };
        self.buckets[bucket] += 1;
        self.total_ns = self.total_ns.saturating_add(value_ns as u128);
        self.count += 1;
        if value_ns > self.max_ns {
            self.max_ns = value_ns;
        }
    }

    fn percentile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((q * self.count as f64).ceil() as u64)
            .max(1)
            .min(self.count);
        let mut running = 0u64;
        for (i, &c) in self.buckets.iter().enumerate() {
            running += c;
            if running >= target {
                let lo = if i == 0 { 0 } else { 1u64 << i };
                let hi = if i == 63 { u64::MAX } else { 1u64 << (i + 1) };
                return lo.saturating_add((hi - lo) / 2);
            }
        }
        self.max_ns
    }

    fn stats(self, name: &'static str, unit: &'static str) -> StageStats {
        if self.count == 0 {
            return StageStats {
                name,
                unit,
                ..StageStats::default()
            };
        }
        let total = self.total_ns.min(u64::MAX as u128) as u64;
        let avg = (self.total_ns / self.count as u128) as u64;
        StageStats {
            name,
            unit,
            count: self.count,
            total,
            avg,
            p50: self.percentile(0.5),
            p95: self.percentile(0.95),
            max: self.max_ns,
        }
    }
}
