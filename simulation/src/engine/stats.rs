use tracing::info;

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

pub struct Histogram {
    buckets: [u64; 64],
    total_ns: u128,
    count: u64,
    max_ns: u64,
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            buckets: [0; 64],
            total_ns: 0,
            count: 0,
            max_ns: 0,
        }
    }

    #[inline(always)]
    pub fn record(&mut self, value_ns: u64) {
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
        let target = ((q * self.count as f64).ceil() as u64).max(1).min(self.count);
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

    pub fn stats(self, name: &'static str, unit: &'static str) -> StageStats {
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
